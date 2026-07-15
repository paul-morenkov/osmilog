use eframe;
use egui::{Pos2, Rect, Sense};
use slotmap::{new_key_type, SecondaryMap, SlotMap};
use std::collections::HashMap;

use crate::gui::canvas_draw::draw_ghost;
use crate::gui::clipboard::Clipboard;
use crate::gui::document::{default_new_circuit_name, CircuitDoc, DocId, Document};
use crate::gui::geometry::{tunnel_shape, Camera, GridPos, ZOOM_SCROLL_SPEED};
use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::history::History;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::properties::PropGuiAction;
use crate::gui::shape::ComponentShape;
use crate::gui::theme::Theme;
use crate::gui::utils::CanvasCtx;
use crate::gui::wiring::{NodeAttach, WireNode, WireNodeKey, WireSegKey, WireSegment, Wiring};
use crate::io::{
    CircuitEntry, CircuitSnapshot, ComponentEntry, LoadError, NodeAttachEntry, NodeEntry,
    ProjectFile, SegEntry, SubcircuitRef, TunnelEntry,
};
use crate::platform;
use crate::sim::circuit::{Circuit, TunnelKey, TunnelRole};
use crate::sim::command::Command;
use crate::sim::component::*;
use crate::sim::value::Value;

// ── Constants ─────────────────────────────────────────────────────────────────

// Shared canvas pixel sizes. pub(crate) because gui::canvas_draw draws with the
// same measurements the hit-testing here uses.
pub(crate) const PIN_RADIUS: f32 = 3.0;
pub(crate) const WIRE_THICKNESS_THIN: f32 = 2.0;
pub(crate) const WIRE_THICKNESS_THICK: f32 = 4.0;
pub(crate) const COMP_STROKE: f32 = 1.5;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("OSMILOG_GIT_SHA");

// ── PlacedTunnel ──────────────────────────────────────────────────────────────

// Visual record for a Tunnel (net label / off-page connector). `label`
// mirrors circuit::Tunnel.label (editing it updates the text and calls
// circuit.rename_tunnel) - Tunnels are the only entity with a user-editable
// label; Components only show hardcoded per-type/pin labels.
pub struct PlacedTunnel {
    pub key: TunnelKey,
    pub label: String,
    pub role: TunnelRole,
    pub grid_pos: GridPos,
    // Tombstone flag; see PlacedComponent::active. A deleted tunnel is flagged
    // inactive rather than removed so its PlacedTunnelKey survives for undo.
    pub active: bool,
}

// ── Selected ──────────────────────────────────────────────────────────────────

// A component, a tunnel, and a wire segment are all selectable canvas entities;
// using one enum (rather than parallel Option fields) avoids a "can two be Some,
// who wins" desync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Component(PlacedCompKey),
    Tunnel(PlacedTunnelKey),
    Wire(WireSegKey),
}

// OsmilogApp::selected's payload: one selected item, or a multi-item bulk
// selection from a rubber-band drag. No `None`/empty variant - that's what
// `Option<Selection>` is for, so "nothing selected" has exactly one
// representation rather than two. A `Bulk` is never constructed empty; an
// empty bulk selection is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Single(Selected),
    Bulk(Vec<Selected>),
}

// ── InteractionMode ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum InteractionMode {
    Idle,
    Placing {
        spec: ComponentSpec,
    },
    PlacingTunnel {
        role: TunnelRole,
    },
    // Drawing a wire (drag = quick elbow, click = add a corner). `points` are
    // the committed grid corners (points[0] = anchor); `dragging` distinguishes
    // finish-on-release from finish-on-click/double-click/Esc.
    WireDraw {
        points: Vec<GridPos>,
        start_attach: NodeAttach,
        cursor: Pos2,
        dragging: bool,
    },
    // Body-drag of the current selection. `items` pairs each dragged key with
    // the grid_pos it had at drag-start. `free_nodes` are the Free-attached
    // WireNodes of any selected wire segment. If not tracked, only wires that are directly
    // connected between two component pins move.
    SelectionDrag {
        items: Vec<(Selected, GridPos)>,
        free_nodes: Vec<(WireNodeKey, GridPos)>,
        drag_origin: Pos2,
    },
    // Rubber-band multi-select from dragging an empty region; `start`/`current`
    // are the box corners (GridPos, so it snaps to grid) - on release,
    // everything inside becomes the (bulk) selection.
    BulkSelect {
        start: GridPos,
        current: GridPos,
    },
}

// ── PinKind ───────────────────────────────────────────────────────────────────

pub(crate) enum PinKind {
    Input,
    Output,
}

// ── OsmilogApp ────────────────────────────────────────────────────────────────

new_key_type! {
    pub struct PlacedCompKey;
    pub struct PlacedTunnelKey;
}

pub struct OsmilogApp {
    // Snapshot of the last copied selection, decoupled from live SlotMap
    // keys so it survives undo/redo and further edits to the copied
    // originals. See gui::clipboard::Clipboard.
    pub clipboard: Clipboard,
    // File > Save/Load I/O errors. Distinct from a Document's own
    // settle_error (a simulation-side problem); the menu bar shows whichever
    // is set, I/O first (see the "Menu bar" section of `ui`).
    pub io_error: Option<String>,
    // Platform-specific file I/O state and orchestration (native OS dialogs vs.
    // browser async pick / Blob download + in-app Save As modal), behind one
    // interface so the call sites below are cfg-free. See `crate::platform` and
    // the `with_io` helper; native's IoState is a ZST, web's holds the async
    // load slot + modal contents.
    io: platform::IoState,
    // Toggles the in-app puffin flamegraph window (Debug menu). puffin
    // scopes are recorded regardless; this just controls whether the
    // viewer is drawn.
    show_profiler: bool,
    // ── Multiple circuits ──────────────────────────────────────────────────
    // Every circuit document held in memory, active included - a single
    // source of truth, with no "parked vs. live" distinction. See
    // gui::document::Document.
    documents: SlotMap<DocId, CircuitDoc>,
    // Stable palette display order for `documents` (SlotMap iteration order is
    // unspecified). Grows as circuits are created.
    doc_order: Vec<DocId>,
    // Which document is currently active. See active()/active_mut().
    active_id: DocId,
    // New-circuit name dialog: Some(buffer) while open (the String doubles as
    // the live text-field contents), None while closed. Mirrors the web Save As
    // modal pattern (platform/web.rs) but lives on the app so native gets it too.
    new_circuit_dialog: Option<String>,
}

impl OsmilogApp {
    // Split out from `new` so tests (and `load_project_file`) can construct
    // a fresh app without an eframe::CreationContext, which isn't
    // constructible outside a running eframe host.
    pub fn empty() -> Self {
        // The single initial "Main" document, active from the start.
        let mut documents = SlotMap::with_key();
        let active_id = documents.insert(CircuitDoc {
            name: "Main".to_string(),
            state: Document::blank(),
        });
        Self {
            clipboard: Clipboard::new(),
            io_error: None,
            #[allow(clippy::default_constructed_unit_structs)]
            io: platform::IoState::default(),
            show_profiler: false,
            documents,
            doc_order: vec![active_id],
            active_id,
            new_circuit_dialog: None,
        }
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        puffin::set_scopes_on(true);
        // `Options` lives in the Context's persistent memory (like styles/widget
        // state), so this sticks for the app's lifetime - no need to re-set it
        // every frame in handle_camera_input.
        cc.egui_ctx
            .options_mut(|o| o.input_options.scroll_zoom_speed = ZOOM_SCROLL_SPEED);
        Self::empty()
    }

    // ── Multiple circuits ──────────────────────────────────────────────────

    // The active document's state. All per-document reads/writes go through
    // these two accessors, indexing straight into the single source of truth
    // (`documents`) rather than a separate set of "live" fields.
    pub(crate) fn active(&self) -> &Document {
        &self.documents[self.active_id].state
    }

    pub(crate) fn active_mut(&mut self) -> &mut Document {
        &mut self.documents[self.active_id].state
    }

    // Make `target` the active document. No-op if `target` is already active.
    // No net rebuild is needed - every document already holds its own settled
    // nets, active or not.
    pub(crate) fn switch_circuit(&mut self, target: DocId) {
        if target == self.active_id {
            return;
        }
        self.active_id = target;
        // Reflect any edits made to child circuits while they were the active
        // document: re-derive every placed subcircuit here against its source.
        self.refresh_subcircuits();
    }

    // Create a new blank circuit document and make it active.
    fn create_circuit_doc(&mut self, name: String) {
        let id = self.documents.insert(CircuitDoc {
            name,
            state: Document::blank(),
        });
        self.doc_order.push(id);
        self.active_id = id;
    }

    // True while a clock run session is active (Playing or Paused). The single
    // gate for the edit lockout: canvas interaction, shortcuts, the Add/Edit
    // menus, File > Load, and the properties panel are all disabled when this
    // is true. Only Stop (which resets sequential state) returns to editable.
    pub fn editing_locked(&self) -> bool {
        self.active().editing_locked()
    }

    // True while live *value* edits must be blocked - an Input's bits, a ROM's
    // or RAM's contents. Unlike the blanket editing_locked(), these are carved
    // out of the lock while Paused: a paused run is frozen structurally, but an
    // Input can still be driven to new stimulus and memory poked between steps,
    // so this is true only while actively Playing. Structural edits (widths,
    // wiring, add/delete) stay gated on editing_locked().
    pub fn value_editing_locked(&self) -> bool {
        self.active().value_editing_locked()
    }

    // Builds the live sim Component (needs the document registry, so this part
    // can't live on Document - see instantiate below) then hands it to
    // Document::place_component for the per-document record/undo bookkeeping.
    fn place_component(&mut self, spec: ComponentSpec, grid_pos: GridPos) -> PlacedCompKey {
        let comp = self.instantiate(&spec);
        self.active_mut().place_component(comp, spec, grid_pos)
    }

    // Applies one intent collected from the properties panel (see
    // gui::properties::show_properties, which only *describes* edits over a
    // read-only &Document). This is the single place those descriptions become
    // mutations - keeping the panel itself decoupled from OsmilogApp.
    pub(crate) fn apply_prop_gui_action(&mut self, action: PropGuiAction) {
        match action {
            PropGuiAction::Reconfigure(key, spec) => self.reconfigure_component(key, spec),
            PropGuiAction::OpenMemory(key, kind) => self.active_mut().memory_editor.open(key, kind),
            PropGuiAction::OpenCircuit(doc) => self.switch_circuit(doc),
            PropGuiAction::SetTunnelLabelLive(key, label) => {
                // Persist the in-progress edit back to the record (the panel's
                // text buffer is re-cloned from it next frame). Same-frame with
                // the render, so the keystroke isn't lost.
                self.active_mut().tunnels[key].label = label;
            }
            PropGuiAction::RenameTunnel(key, label) => {
                let doc = self.active_mut();
                let tunnel_key = doc.tunnels[key].key;
                // Read the old label from the circuit (the record's copy was
                // already updated live) to capture undo's restore value.
                let old_label = doc.circuit.tunnels.get(tunnel_key).map(|t| t.label.clone());
                doc.history.begin_batch();
                doc.apply(Command::RenameTunnel {
                    tunnel: tunnel_key,
                    new_label: label.clone(),
                });
                // Record the record-side label change's undo (the Sim
                // RenameTunnel above only reverses the circuit's copy).
                if let Some(old) = old_label {
                    doc.history
                        .push_gui(GuiUndoAction::SetTunnelLabel { key, label: old });
                }
                doc.tunnels[key].label = label;
                doc.history.end_batch();
                let result = doc.circuit.settle();
                doc.record_settle_result(result);
            }
            PropGuiAction::Delete(sel) => match sel {
                Selected::Component(key) => self.active_mut().delete_component(key),
                Selected::Tunnel(key) => self.active_mut().delete_tunnel(key),
                Selected::Wire(seg) => self.active_mut().delete_wire(seg),
            },
        }
    }

    // Thin registry-glue wrapper over Document::reconfigure_component: builds
    // the new component via instantiate (which needs the document registry, so
    // it can't run inside Document) and hands it to the active document, which
    // owns all the record/wiring/undo bookkeeping.
    pub(crate) fn reconfigure_component(&mut self, pc_key: PlacedCompKey, new_spec: ComponentSpec) {
        let new_comp = self.instantiate(&new_spec);
        self.active_mut()
            .reconfigure_component(pc_key, new_spec, new_comp);
    }

    // ── Subcircuits ───────────────────────────────────────────────────────────
    //
    // Builds a live sim Component from a ComponentSpec. Identical to
    // spec.to_component() for every primitive type; a Subcircuit spec instead
    // has its inner Circuit built from the referenced document (which
    // to_component can't do - it has no document registry). This is the one
    // spec->component build path the GUI uses (place_component /
    // reconfigure_component), so subcircuits always get a real inner circuit.
    pub(crate) fn instantiate(&self, spec: &ComponentSpec) -> Component {
        let mut visited = Vec::new();
        self.instantiate_with(spec, &mut visited)
    }

    // `visited` breaks any accidental reference cycle (placement guards against
    // real ones via would_cycle): a document already on the stack yields an
    // empty placeholder instead of recursing forever.
    fn instantiate_with(&self, spec: &ComponentSpec, visited: &mut Vec<DocId>) -> Component {
        match spec {
            ComponentSpec::Subcircuit {
                doc,
                input_widths,
                output_widths,
                ..
            } => {
                if visited.contains(doc) {
                    return Component::subcircuit_placeholder(
                        input_widths.len(),
                        output_widths.len(),
                    );
                }
                visited.push(*doc);
                let (inner, inputs, outputs) = self.build_doc_circuit(*doc, visited);
                visited.pop();
                // The outer pin arity is always the cached interface. If the
                // referenced document isn't fully available (e.g. mid-load, a
                // forward reference to a not-yet-populated doc, or a null/unbound
                // `doc`), its derived boundary won't match - fall back to a
                // correctly-sized placeholder so wiring to these pins never goes
                // out of bounds. refresh_subcircuits rebuilds the real inner once
                // every document is populated.
                if inputs.len() == input_widths.len() && outputs.len() == output_widths.len() {
                    Component::subcircuit(inner, inputs, outputs)
                } else {
                    Component::subcircuit_placeholder(input_widths.len(), output_widths.len())
                }
            }
            _ => spec.to_component(),
        }
    }

    // Builds a fresh standalone Circuit from a referenced document's records
    // (its Document in the documents slotmap), translating placed
    // components/tunnels + the wiring graph the same way Document::
    // rebuild_circuit translates its own live doc - but into a new Circuit
    // rather than the active document's, untracked, and recursing through
    // instantiate_with for nested subcircuits. Returns the inner Input/Output
    // component keys ordered top-down (then left-to-right), which is the pin
    // order the subcircuit component exposes and its shape lays out.
    fn build_doc_circuit(
        &self,
        doc: DocId,
        visited: &mut Vec<DocId>,
    ) -> (Circuit, Vec<CompKey>, Vec<CompKey>) {
        // Every document's records live directly in its CircuitDoc::state,
        // active or not.
        let Some(cdoc) = self.documents.get(doc) else {
            return (Circuit::new(), Vec::new(), Vec::new());
        };
        let state = &cdoc.state;

        let mut circuit = Circuit::new();
        let mut comp_map: SecondaryMap<PlacedCompKey, CompKey> = SecondaryMap::new();
        for (pck, pc) in state.components.iter().filter(|(_, pc)| pc.active) {
            let comp = self.instantiate_with(&pc.spec, visited);
            comp_map.insert(pck, circuit.add_component(comp));
        }

        let mut tunnel_map: SecondaryMap<PlacedTunnelKey, TunnelKey> = SecondaryMap::new();
        for (ptk, pt) in state.tunnels.iter().filter(|(_, pt)| pt.active) {
            tunnel_map.insert(ptk, circuit.add_tunnel(pt.label.clone(), pt.role));
        }

        for group in state.wiring.groups() {
            let pins: Vec<(CompKey, PinId)> = group
                .pins
                .iter()
                .filter_map(|&(pck, pin)| comp_map.get(pck).map(|&ck| (ck, pin)))
                .collect();
            let Some(&(anchor_comp, anchor_pin)) = pins.first() else {
                continue;
            };
            for &(comp, pin) in &pins[1..] {
                circuit.link(anchor_comp, anchor_pin, comp, pin);
            }
            for &ptk in &group.tunnels {
                if let Some(&tk) = tunnel_map.get(ptk) {
                    circuit.link_tunnel(tk, anchor_comp, anchor_pin);
                }
            }
        }

        // Boundary pins are ordered top-down (then left-to-right) by the
        // Input/Output components' grid positions.
        let mut inputs: Vec<(GridPos, CompKey)> = Vec::new();
        let mut outputs: Vec<(GridPos, CompKey)> = Vec::new();
        for (pck, pc) in state.components.iter().filter(|(_, pc)| pc.active) {
            match pc.spec {
                ComponentSpec::Input(_) => inputs.push((pc.grid_pos, comp_map[pck])),
                ComponentSpec::Output => outputs.push((pc.grid_pos, comp_map[pck])),
                _ => {}
            }
        }
        inputs.sort_by_key(|(g, _)| (g.y, g.x));
        outputs.sort_by_key(|(g, _)| (g.y, g.x));

        let _ = circuit.settle();
        (
            circuit,
            inputs.into_iter().map(|(_, k)| k).collect(),
            outputs.into_iter().map(|(_, k)| k).collect(),
        )
    }

    // The interface a subcircuit component exposes for a given document: its
    // display name plus the per-pin widths of the boundary Input/Output
    // components (top-down). Cached into the ComponentSpec::Subcircuit so the
    // `&self` spec methods (n_inputs/size/shape/...) need no document registry;
    // refreshed on switch-back (refresh_subcircuits).
    fn derive_subcircuit_interface(&self, doc: DocId) -> (String, Vec<u8>, Vec<u8>) {
        let name = self
            .documents
            .get(doc)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        let mut visited = Vec::new();
        let (circuit, in_keys, out_keys) = self.build_doc_circuit(doc, &mut visited);
        let input_widths = in_keys
            .iter()
            .map(|&k| {
                circuit
                    .components
                    .get(k)
                    .and_then(|c| c.output_width(OutIdx(0)))
                    .unwrap_or(1)
            })
            .collect();
        let output_widths = out_keys
            .iter()
            .map(|&k| match circuit.read_output(k) {
                Value::Fixed { width, .. } => width,
                _ => 1,
            })
            .collect();
        (name, input_widths, output_widths)
    }

    // Builds the Subcircuit spec for placing `doc` as a component, deriving its
    // cached interface now.
    fn subcircuit_spec(&self, doc: DocId) -> ComponentSpec {
        let (name, input_widths, output_widths) = self.derive_subcircuit_interface(doc);
        ComponentSpec::Subcircuit {
            doc,
            name,
            input_widths,
            output_widths,
        }
    }

    // Rebuilds one placed subcircuit's live inner Circuit in place (same
    // CompKey, same outer pins), so inner edits that didn't change the boundary
    // are reflected. Used by refresh_subcircuits for the common case.
    fn rebuild_subcircuit_inner(&mut self, pck: PlacedCompKey) {
        let ComponentSpec::Subcircuit { doc, .. } = self.active().components[pck].spec else {
            return;
        };
        let comp_key = self.active().components[pck].key;
        let mut visited = Vec::new();
        let (inner, inputs, outputs) = self.build_doc_circuit(doc, &mut visited);
        if let Some(comp) = self.active_mut().circuit.components.get_mut(comp_key) {
            if let Logic::Sub(sub) = &mut comp.logic {
                sub.inner = inner;
                sub.inputs = inputs;
                sub.outputs = outputs;
            }
        }
    }

    // Reconciles every placed subcircuit in the active document against its
    // referenced document, called after a switch makes this document active so
    // edits to a child circuit show up here. If the boundary changed (pin
    // count), reconfigure_component rebuilds the whole record (pruning wires to
    // dropped pins, positional binding); otherwise the inner circuit is rebuilt
    // in place and the cached name/widths refreshed. Finishes with a single
    // rebuild_circuit so the re-derived inner outputs settle outward.
    fn refresh_subcircuits(&mut self) {
        let subs: Vec<(PlacedCompKey, DocId)> = self
            .active()
            .active_components()
            .filter_map(|(pck, pc)| match &pc.spec {
                ComponentSpec::Subcircuit { doc, .. } => Some((pck, *doc)),
                _ => None,
            })
            .collect();

        let mut rebuilt_any = false;
        for (pck, doc) in subs {
            let (name, input_widths, output_widths) = self.derive_subcircuit_interface(doc);
            let (old_in, old_out, old_name) = match &self.active().components[pck].spec {
                ComponentSpec::Subcircuit {
                    input_widths,
                    output_widths,
                    name,
                    ..
                } => (input_widths.len(), output_widths.len(), name.clone()),
                _ => continue,
            };

            if old_in != input_widths.len() || old_out != output_widths.len() {
                // Boundary changed: full reconfigure (prunes stale wires, new
                // shape, rebuilt inner circuit). It rebuild_circuits itself.
                self.reconfigure_component(
                    pck,
                    ComponentSpec::Subcircuit {
                        doc,
                        name,
                        input_widths,
                        output_widths,
                    },
                );
            } else {
                // Same boundary: refresh the cached name/widths (display only;
                // shape is unchanged since pin counts match) and rebuild the
                // inner circuit in place.
                if old_name != name {
                    self.active_mut().components[pck].spec = ComponentSpec::Subcircuit {
                        doc,
                        name,
                        input_widths,
                        output_widths,
                    };
                }
                self.rebuild_subcircuit_inner(pck);
                rebuilt_any = true;
            }
        }

        if rebuilt_any {
            self.active_mut().rebuild_circuit();
        }
    }

    // The documents referenced (as subcircuits) directly by `doc`'s placed
    // components. Every document's records live directly in its
    // CircuitDoc::state, active or not, so no active-doc special case is
    // needed here.
    fn doc_references(&self, doc: DocId) -> Vec<DocId> {
        self.documents
            .get(doc)
            .into_iter()
            .flat_map(|d| d.state.components.values())
            .filter(|pc| pc.active)
            .filter_map(|pc| match &pc.spec {
                ComponentSpec::Subcircuit { doc, .. } => Some(*doc),
                _ => None,
            })
            .collect()
    }

    // Whether placing `target` as a subcircuit in the active document would
    // create a cycle: true if target is the active doc itself, or target
    // already (transitively) references the active doc.
    fn would_cycle(&self, target: DocId) -> bool {
        if target == self.active_id {
            return true;
        }
        let mut stack = vec![target];
        let mut seen: Vec<DocId> = Vec::new();
        while let Some(d) = stack.pop() {
            if d == self.active_id {
                return true;
            }
            if seen.contains(&d) {
                continue;
            }
            seen.push(d);
            stack.extend(self.doc_references(d));
        }
        false
    }

    // Repositions the component's wire-anchor nodes to its current pin grid
    // positions (after a move or reconfigure). Segments to them stretch.
    // ── Save / load ──────────────────────────────────────────────────────

    // Serializes the whole workspace - every circuit document, not just the
    // active one - into a ProjectFile, with each placed subcircuit's
    // cross-circuit link emitted as an index into `circuits` (see
    // `circuit_entry_of`). `doc_order` fixes both the emitted circuit order and
    // the index every reference resolves against.
    pub fn to_project_file(&self) -> ProjectFile {
        let doc_index: HashMap<DocId, usize> = self
            .doc_order
            .iter()
            .enumerate()
            .map(|(i, &d)| (d, i))
            .collect();
        let active = doc_index[&self.active_id];
        let circuits = self
            .doc_order
            .iter()
            .map(|&doc| self.circuit_entry_of(doc, &doc_index))
            .collect();
        ProjectFile::new(active, circuits)
    }

    // Builds one document's CircuitEntry. Every document's records live
    // directly in its CircuitDoc::state, active or not. Subcircuit references
    // map each placed Subcircuit component (by its emitted index) to the index
    // of the document it references, via `doc_index`.
    fn circuit_entry_of(&self, doc: DocId, doc_index: &HashMap<DocId, usize>) -> CircuitEntry {
        let name = self.documents[doc].name.clone();
        let state = &self.documents[doc].state;
        let (components_map, tunnels_map, wiring) =
            (&state.components, &state.tunnels, &state.wiring);
        let (snapshot, comp_index) = extract_records(components_map, tunnels_map, wiring);
        let subcircuits = components_map
            .iter()
            .filter(|(_, pc)| pc.active)
            .filter_map(|(pck, pc)| match &pc.spec {
                ComponentSpec::Subcircuit { doc, .. } => {
                    doc_index.get(doc).map(|&circuit| SubcircuitRef {
                        component: comp_index[&pck],
                        circuit,
                    })
                }
                _ => None,
            })
            .collect();
        CircuitEntry {
            name,
            snapshot,
            subcircuits,
        }
    }

    // Replaces the whole workspace with the circuits described by `file`,
    // restoring its active document. Validates first so a malformed file is
    // rejected before any existing state is touched.
    //
    // Every document is allocated (blank) up front, so a stable DocId exists for
    // each circuit before any records are placed - subcircuit references then
    // resolve by index regardless of the order documents are populated in.
    // Circuits are loaded one at a time by making each one active in turn
    // (make_live_for_load) and installing its records, which reuses the
    // ordinary placement machinery. A placed subcircuit's inner Circuit is left
    // as a placeholder here and rebuilt against its (now fully-populated)
    // referenced document by the final `refresh_subcircuits`; other documents'
    // subcircuits are likewise rebuilt when they're later switched to.
    pub fn load_project_file(&mut self, file: &ProjectFile) -> Result<(), LoadError> {
        file.validate()?;

        let mut documents: SlotMap<DocId, CircuitDoc> = SlotMap::with_key();
        let doc_ids: Vec<DocId> = file
            .circuits
            .iter()
            .map(|c| documents.insert(CircuitDoc::blank(c.name.clone())))
            .collect();

        self.documents = documents;
        self.doc_order = doc_ids.clone();
        self.io_error = None;
        self.active_id = doc_ids[0];

        for (i, entry) in file.circuits.iter().enumerate() {
            self.make_live_for_load(doc_ids[i]);
            self.load_circuit_entry(entry, &doc_ids);
        }

        // Restore the saved active document and reconcile its placed subcircuits
        // against the now fully-populated referenced documents.
        self.make_live_for_load(doc_ids[file.active]);
        self.active_mut().selected = None;
        self.active_mut().mode = InteractionMode::Idle;
        self.refresh_subcircuits();
        self.active_mut().rebuild_circuit();
        Ok(())
    }

    // Makes `target` the active document, without the `refresh_subcircuits` a
    // normal `switch_circuit` runs - during a load the referenced documents
    // aren't all populated yet, so subcircuit rebuilding is deferred to the end
    // of `load_project_file`. Just an active_id reassignment: every document
    // already has its own (blank, freshly allocated) Document in the slotmap.
    fn make_live_for_load(&mut self, target: DocId) {
        self.active_id = target;
    }

    // Loads one circuit's records into the (blank) active document, then
    // rebuilds its nets. Assumes the active document is the fresh blank state
    // for this circuit (as arranged by `load_project_file`).
    fn load_circuit_entry(&mut self, entry: &CircuitEntry, doc_ids: &[DocId]) {
        self.install_circuit_records(&entry.snapshot, &entry.subcircuits, doc_ids);
        self.active_mut().rebuild_circuit();
        // Placement records undo entries that loading a fresh document should
        // not carry.
        self.active_mut().history = History::default();
    }

    // Places one circuit's records (components, tunnels, wire nodes/segments)
    // into the active document and re-binds subcircuit references, for the
    // per-circuit `load_circuit_entry`. Does not rebuild nets - the caller does,
    // once its records are in.
    fn install_circuit_records(
        &mut self,
        snapshot: &CircuitSnapshot,
        subcircuits: &[SubcircuitRef],
        doc_ids: &[DocId],
    ) {
        // File indices -> the freshly placed GUI keys (wiring nodes reference
        // components/tunnels by these).
        let comp_keys: Vec<PlacedCompKey> = snapshot
            .components
            .iter()
            .map(|entry| self.place_component(entry.spec.clone(), entry.grid_pos))
            .collect();

        // Re-bind each Subcircuit's `doc` (serde-skipped, so it loaded as a null
        // DocId) to the DocId allocated for the circuit it references.
        for sub in subcircuits {
            if let (Some(&pck), Some(&doc)) =
                (comp_keys.get(sub.component), doc_ids.get(sub.circuit))
            {
                if let ComponentSpec::Subcircuit { doc: d, .. } =
                    &mut self.active_mut().components[pck].spec
                {
                    *d = doc;
                }
            }
        }

        let tunnel_keys: Vec<PlacedTunnelKey> = snapshot
            .tunnels
            .iter()
            .map(|entry| {
                self.active_mut().place_tunnel_labeled(
                    entry.label.clone(),
                    entry.role,
                    entry.grid_pos,
                )
            })
            .collect();

        let node_keys: Vec<_> = snapshot
            .nodes
            .iter()
            .map(|entry| {
                let attach = match entry.attach {
                    NodeAttachEntry::Free => NodeAttach::Free,
                    NodeAttachEntry::Pin {
                        comp,
                        is_input,
                        pin_index,
                    } => {
                        let pin = if is_input {
                            PinId::input(pin_index)
                        } else {
                            PinId::output(pin_index)
                        };
                        NodeAttach::Pin(comp_keys[comp], pin)
                    }
                    NodeAttachEntry::Tunnel { tunnel } => NodeAttach::Tunnel(tunnel_keys[tunnel]),
                };
                self.active_mut().wiring.nodes.insert(WireNode {
                    pos: entry.pos,
                    attach,
                    active: true,
                })
            })
            .collect();

        for s in &snapshot.segments {
            self.active_mut().wiring.segments.insert(WireSegment {
                a: node_keys[s.a],
                b: node_keys[s.b],
                active: true,
            });
        }
    }

    // Draws the "New Circuit" name dialog while `new_circuit_dialog` is Some.
    // The Option doubles as open-state and the live text buffer, mirroring the
    // web Save As modal (platform/web.rs); living here (not in the web backend)
    // means native gets the dialog too. On Create it makes a new active blank
    // circuit; Cancel / ✕ / an empty-after-trim name that's still confirmed all
    // fall back sensibly. Driven once per frame from `ui()`.
    fn create_new_circuit_dialog(&mut self, ctx: &egui::Context) {
        let Some(name) = &mut self.new_circuit_dialog else {
            return;
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new("New Circuit")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    let resp = ui.text_edit_singleline(name);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirmed = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Create").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });

        if confirmed {
            let trimmed = name.trim();
            let final_name = if trimmed.is_empty() {
                default_new_circuit_name(&self.documents)
            } else {
                trimmed.to_string()
            };
            self.create_circuit_doc(final_name);
        }
        if !open || confirmed || cancelled {
            self.new_circuit_dialog = None;
        }
    }

    // Snapshots the current selection onto the clipboard. No-op if nothing
    // is selected. Read-only: never touches history.
    fn copy_selection(&mut self) {
        // Indexed directly (not via active()) so this borrows only
        // self.documents, leaving self.clipboard free for the &mut borrow
        // .copy() below needs - active() is an opaque method call the borrow
        // checker can't see through as a disjoint field borrow.
        let doc = &self.documents[self.active_id].state;
        let items: Vec<Selected> = match &doc.selected {
            None => return,
            Some(Selection::Single(s)) => vec![*s],
            Some(Selection::Bulk(v)) => v.clone(),
        };
        self.clipboard
            .copy(&doc.components, &doc.tunnels, &doc.wiring, &items);
    }

    // Materializes the clipboard's (offset) snapshot as new components,
    // tunnels, and wiring, as one undoable batch; the pasted items become
    // the new selection. No-op if the clipboard is empty.
    fn paste_clipboard(&mut self) {
        let Some(file) = self.clipboard.plan_paste() else {
            return;
        };
        self.active_mut().history.begin_batch();

        // Snapshot indices -> the freshly placed GUI keys, mirroring
        // install_circuit_records (wiring nodes reference components/tunnels by
        // these).
        let comp_keys: Vec<PlacedCompKey> = file
            .components
            .iter()
            .map(|entry| self.place_component(entry.spec.clone(), entry.grid_pos))
            .collect();

        let tunnel_keys: Vec<PlacedTunnelKey> = file
            .tunnels
            .iter()
            .map(|entry| {
                self.active_mut().place_tunnel_labeled(
                    entry.label.clone(),
                    entry.role,
                    entry.grid_pos,
                )
            })
            .collect();

        let nodes: Vec<(GridPos, NodeAttach)> = file
            .nodes
            .iter()
            .map(|entry| {
                let attach = match entry.attach {
                    NodeAttachEntry::Free => NodeAttach::Free,
                    NodeAttachEntry::Pin {
                        comp,
                        is_input,
                        pin_index,
                    } => {
                        let pin = if is_input {
                            PinId::input(pin_index)
                        } else {
                            PinId::output(pin_index)
                        };
                        NodeAttach::Pin(comp_keys[comp], pin)
                    }
                    NodeAttachEntry::Tunnel { tunnel } => NodeAttach::Tunnel(tunnel_keys[tunnel]),
                };
                (entry.pos, attach)
            })
            .collect();
        let segments: Vec<(usize, usize)> = file.segments.iter().map(|s| (s.a, s.b)).collect();

        let doc = self.active_mut();
        let (_, seg_keys, delta) = doc.wiring.add_subgraph(&nodes, &segments);
        doc.edit_wiring(delta);
        doc.rebuild_circuit();

        let mut new_selection: Vec<Selected> = Vec::new();
        new_selection.extend(comp_keys.into_iter().map(Selected::Component));
        new_selection.extend(tunnel_keys.into_iter().map(Selected::Tunnel));
        new_selection.extend(seg_keys.into_iter().map(Selected::Wire));
        doc.selected = match new_selection.len() {
            0 => None,
            1 => Some(Selection::Single(new_selection[0])),
            _ => Some(Selection::Bulk(new_selection)),
        };

        doc.history.end_batch();
    }

    // Runs `f` with the platform `IoState` temporarily moved out of `self`, so
    // the IO methods can take a `&mut OsmilogApp` (to serialize, install a
    // loaded file, or set an error) without aliasing `self.io`. Both backends'
    // `IoState` is `Default`, so the take/restore is cheap - web's is an Rc +
    // Option, native's is a ZST - and it keeps every File-menu / per-frame call
    // site cfg-free.
    fn with_io<R>(&mut self, f: impl FnOnce(&mut platform::IoState, &mut Self) -> R) -> R {
        let mut io = std::mem::take(&mut self.io);
        let r = f(&mut io, self);
        self.io = io;
        r
    }

    // ── Menu bar ──────────────────────────────────────────────────────────
    fn show_menu_bar(&mut self, ui: &mut egui::Ui, theme: Theme) {
        // A run session (Playing/Paused) makes the whole editor read-only; the
        // structural menus, Load, and the properties panel are disabled while
        // it's true (Save/Debug/clock transport stay live).
        let locked = self.editing_locked();
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Save").clicked() {
                        // Native opens the OS "Save As" dialog and writes
                        // synchronously; web opens an in-app filename modal
                        // (completed by drive_save_dialog on a later frame).
                        self.with_io(|io, app| io.request_save(app));
                        ui.close();
                    }
                    if ui.add_enabled(!locked, egui::Button::new("Load")).clicked() {
                        // Native picks + reads + installs synchronously; web
                        // kicks off an async task whose result lands in the IO
                        // state and is installed by poll_pending_load later.
                        self.with_io(|io, app| io.request_load(app));
                        ui.close();
                    }
                });
                ui.add_enabled_ui(!locked, |ui| {
                    ui.menu_button("Edit", |ui| {
                        if ui
                            .add_enabled(
                                self.active().history.can_undo(),
                                egui::Button::new("Undo"),
                            )
                            .clicked()
                        {
                            self.active_mut().undo();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.active().history.can_redo(),
                                egui::Button::new("Redo"),
                            )
                            .clicked()
                        {
                            self.active_mut().redo();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.active().selected.is_some(),
                                egui::Button::new("Copy"),
                            )
                            .clicked()
                        {
                            self.copy_selection();
                            ui.close();
                        }
                        if ui
                            .add_enabled(!self.clipboard.is_empty(), egui::Button::new("Paste"))
                            .clicked()
                        {
                            self.paste_clipboard();
                            ui.close();
                        }
                    });
                });
                ui.menu_button("Debug", |ui| {
                    ui.checkbox(&mut self.show_profiler, "Profiler");
                });
                ui.separator();
                self.active_mut().show_clock_controls(ui);
                // I/O errors take priority (they're rarer and more actionable);
                // otherwise show the active document's own settle() error.
                if let Some(err) = self
                    .io_error
                    .as_ref()
                    .or(self.active().settle_error.as_ref())
                {
                    ui.colored_label(theme.error_text, err);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("v{APP_VERSION} ({GIT_SHA})"));
                });
            })
        });
    }

    // ── Component palette (top half of the left panel) ────────────────────
    // The whole palette is disabled during a run session (same lock as the
    // structural menus and the properties panel below it).
    fn show_component_palette(&mut self, ui: &mut egui::Ui) {
        let locked = self.editing_locked();
        ui.add_enabled_ui(!locked, |ui| {
            // User-created circuits: one selectable entry per document, with a
            // "+" on the header row to create a new one. Selecting an entry
            // switches the whole editing session to that circuit. Uses a custom
            // CollapsingState so the "+" button can sit on the header row.
            let hdr_id = ui.make_persistent_id("user_created_hdr");
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                hdr_id,
                true,
            )
            .show_header(ui, |ui| {
                ui.label("User Created");
                if ui.small_button("+").clicked() {
                    self.new_circuit_dialog = Some(default_new_circuit_name(&self.documents));
                }
            })
            .body(|ui| {
                // Single click places the circuit as a subcircuit on the
                // current canvas (a ghost that follows the cursor; nothing is
                // dropped until a canvas click). Double click opens it for
                // editing. A double click's first click only *enters* placing
                // mode, which the second click cancels before switching - so no
                // stray component is ever placed. A circuit that would nest into
                // itself (directly or transitively) can't be placed. Targets are
                // recorded and acted on after the loop, so the read-borrow of
                // `doc_order`/`documents` doesn't overlap the &mut self calls.
                let mut switch_target = None;
                let mut place_target = None;
                for &doc_id in &self.doc_order {
                    let cyclic = self.would_cycle(doc_id);
                    let resp =
                        ui.selectable_label(doc_id == self.active_id, &self.documents[doc_id].name);
                    let resp = resp.on_hover_text(if cyclic {
                        "Can't nest: would create a cycle"
                    } else {
                        "Click to place as subcircuit · double-click to edit"
                    });
                    if resp.double_clicked() {
                        switch_target = Some(doc_id);
                    } else if resp.clicked() && !cyclic {
                        place_target = Some(doc_id);
                    }
                }
                if let Some(target) = switch_target {
                    // Cancel any placing started by this double click's first
                    // click, so the parent doc isn't parked mid-placement.
                    self.active_mut().mode = InteractionMode::Idle;
                    self.switch_circuit(target);
                } else if let Some(doc) = place_target {
                    let spec = self.subcircuit_spec(doc);
                    self.active_mut().mode = InteractionMode::Placing { spec };
                }
            });

            if ui.button("Input").clicked() {
                self.active_mut().mode = InteractionMode::Placing {
                    spec: ComponentSpec::Input(Input { bits: 0, width: 1 }),
                };
            }
            if ui.button("Constant").clicked() {
                self.active_mut().mode = InteractionMode::Placing {
                    spec: ComponentSpec::Constant(Constant { bits: 0, width: 1 }),
                };
            }
            if ui.button("Output").clicked() {
                self.active_mut().mode = InteractionMode::Placing {
                    spec: ComponentSpec::Output,
                };
            }
            egui::CollapsingHeader::new("Gates").show(ui, |ui| {
                let gates = [
                    ("AND", GateOp::And, 2),
                    ("OR", GateOp::Or, 2),
                    ("XOR", GateOp::Xor, 2),
                    ("NAND", GateOp::Nand, 2),
                    ("NOR", GateOp::Nor, 2),
                    ("NOT", GateOp::Not, 1),
                ];
                for (name, op, n) in gates {
                    if ui.button(name).clicked() {
                        self.active_mut().mode = InteractionMode::Placing {
                            spec: ComponentSpec::Gate(Gate {
                                op,
                                n_inputs: n,
                                width: 1,
                            }),
                        };
                    }
                }
            });
            egui::CollapsingHeader::new("Plexers").show(ui, |ui| {
                if ui.button("Mux").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Mux(Mux {
                            data_width: 1,
                            sel_width: 1,
                        }),
                    };
                }
                if ui.button("Demux").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Demux(Demux {
                            data_width: 1,
                            sel_width: 1,
                        }),
                    };
                }
                if ui.button("Splitter").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Splitter {
                            width: 2,
                            arm_bits: vec![vec![0], vec![1]],
                            direction: FanDirection::Right,
                        },
                    };
                }
                if ui.button("Encoder").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Encoder(Encoder { sel_width: 1 }),
                    };
                }
            });
            egui::CollapsingHeader::new("Arithmetic").show(ui, |ui| {
                if ui.button("Adder").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Adder(Adder { data_width: 1 }),
                    };
                }
                if ui.button("Subtractor").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Subtractor(Subtractor { data_width: 1 }),
                    };
                }
                if ui.button("Multiplier").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Multiplier(Multiplier { data_width: 1 }),
                    };
                }
                if ui.button("Divider").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Divider(Divider { data_width: 1 }),
                    };
                }
                if ui.button("Comparator").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Comparator(Comparator { data_width: 1 }),
                    };
                }
            });
            egui::CollapsingHeader::new("Memory").show(ui, |ui| {
                if ui.button("Register").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Reg(RegConf { data_width: 1 }),
                    };
                }
                if ui.button("Shift Register").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::ShiftReg(ShiftRegConf {
                            data_width: 1,
                            num_stages: 4,
                            parallel_load: false,
                        }),
                    };
                }
                if ui.button("Counter").clicked() {
                    let data_width = 1;
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Counter(CounterConf {
                            data_width,
                            max_value: Value::mask(data_width),
                            overflow_action: OverflowAction::default(),
                        }),
                    };
                }
                egui::CollapsingHeader::new("Flip-Flop").show(ui, |ui| {
                    if ui.button("D Flip-Flop").clicked() {
                        self.active_mut().mode = InteractionMode::Placing {
                            spec: ComponentSpec::DFlipFlop(DFlipFlopConf),
                        };
                    }
                    if ui.button("T Flip-Flop").clicked() {
                        self.active_mut().mode = InteractionMode::Placing {
                            spec: ComponentSpec::TFlipFlop(TFlipFlopConf),
                        };
                    }
                    if ui.button("JK Flip-Flop").clicked() {
                        self.active_mut().mode = InteractionMode::Placing {
                            spec: ComponentSpec::JKFlipFlop(JKFlipFlopConf),
                        };
                    }
                    if ui.button("SR Flip-Flop").clicked() {
                        self.active_mut().mode = InteractionMode::Placing {
                            spec: ComponentSpec::SRFlipFlop(SRFlipFlopConf),
                        };
                    }
                });
                if ui.button("ROM").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Rom(Rom::new(8, 8)),
                    };
                }
                if ui.button("RAM").clicked() {
                    self.active_mut().mode = InteractionMode::Placing {
                        spec: ComponentSpec::Ram(Ram::new(8, 8, ReadBehavior::default())),
                    };
                }
            });
            egui::CollapsingHeader::new("Tunnel").show(ui, |ui| {
                if ui.button("Feed").clicked() {
                    self.active_mut().mode = InteractionMode::PlacingTunnel {
                        role: TunnelRole::Feed,
                    };
                }
                if ui.button("Pull").clicked() {
                    self.active_mut().mode = InteractionMode::PlacingTunnel {
                        role: TunnelRole::Pull,
                    };
                }
            });
        });
    }

    // ── Canvas drawing ────────────────────────────────────────────────────
    // ── Canvas pan / zoom ─────────────────────────────────────────────────
    // Middle-mouse drag pans; Ctrl(+Cmd)+scroll zooms toward the cursor. Both
    // gestures are independent of the primary-button gestures the interaction
    // modes handle (`drag_started`/`clicked`/`drag_delta` are primary-only), so
    // this runs as a standalone pre-step in `ui()`.
    // ── Global canvas keyboard shortcuts ─────────────────────────────────
    // Escape (cancel drag/wire-draw, clear selection), Delete/Backspace, and
    // Undo/Redo. Must run before `handle_canvas_interaction` reads `self.mode`
    // in the same frame, since Escape can reset it to Idle.
    fn handle_canvas_shortcuts(&mut self, ctx: &egui::Context) {
        puffin::profile_function!();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            let doc = self.active_mut();
            match &doc.mode {
                InteractionMode::SelectionDrag {
                    items, free_nodes, ..
                } => {
                    for (key, original_grid_pos) in items {
                        match key {
                            Selected::Component(k) => {
                                doc.components[*k].grid_pos = *original_grid_pos
                            }
                            Selected::Tunnel(k) => doc.tunnels[*k].grid_pos = *original_grid_pos,
                            Selected::Wire(_) => {}
                        }
                    }
                    for (key, original_pos) in free_nodes {
                        doc.wiring.nodes[*key].pos = *original_pos;
                    }
                }
                // Esc while drawing commits the polyline drawn so far as a
                // dangling run (end in empty space), matching the double-click
                // finish; nothing to commit if only the anchor exists.
                InteractionMode::WireDraw {
                    points,
                    start_attach,
                    ..
                } if points.len() >= 2 => {
                    let (points, start_attach) = (points.clone(), *start_attach);
                    doc.commit_wire_route(points, start_attach, NodeAttach::Free);
                }
                // BulkSelect: Esc cancels the in-progress rubber-band (the
                // trailing reset to Idle handles it) alongside clearing any
                // existing bulk selection below.
                _ => {}
            }
            if matches!(doc.selected, Some(Selection::Bulk(_))) {
                doc.selected = None;
            }
            doc.mode = InteractionMode::Idle;
        }

        // Backspace/Delete removes the current selection (bulk selection
        // takes priority). Guarded on widget focus so it edits a focused
        // text field instead of deleting, and on the clock lock so no editing
        // shortcut fires during a run session (Playing/Paused).
        let editing_text = ctx.memory(|m| m.focused().is_some());
        let edits_blocked = editing_text || self.editing_locked();
        let delete_pressed =
            ctx.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete));
        if delete_pressed && !edits_blocked {
            let doc = self.active_mut();
            match &doc.selected {
                Some(Selection::Bulk(_)) => doc.delete_bulk(),
                Some(Selection::Single(sel)) => match *sel {
                    Selected::Component(k) => doc.delete_component(k),
                    Selected::Tunnel(k) => doc.delete_tunnel(k),
                    Selected::Wire(seg) => doc.delete_wire(seg),
                },
                None => {}
            }
        }

        // Undo (Ctrl/Cmd+Z) / redo (Ctrl/Cmd+Y or Ctrl/Cmd+Shift+Z). Same
        // focus/lock guard as delete so the shortcuts don't fire while typing in
        // the tunnel-label field (where Ctrl+Z should edit text) or mid-run.
        if !edits_blocked {
            let (undo, redo) = ctx.input(|i| {
                let cmd = i.modifiers.command;
                let z = i.key_pressed(egui::Key::Z);
                let y = i.key_pressed(egui::Key::Y);
                let undo = cmd && !i.modifiers.shift && z;
                let redo = cmd && (y || (i.modifiers.shift && z));
                (undo, redo)
            });
            if undo {
                self.active_mut().undo();
            } else if redo {
                self.active_mut().redo();
            }
        }

        // Copy (Ctrl/Cmd+C) / Paste (Ctrl/Cmd+V). Same focus/lock guard as
        // delete/undo/redo so the shortcuts don't fire while typing in the
        // tunnel-label field (where Ctrl+C/V should edit text) or mid-run.
        if !edits_blocked {
            let (copy, paste) = ctx.input(|i| {
                let cmd = i.modifiers.command;
                (
                    cmd && i.key_pressed(egui::Key::C),
                    cmd && i.key_pressed(egui::Key::V),
                )
            });
            if copy {
                self.copy_selection();
            } else if paste {
                self.paste_clipboard();
            }
        }
    }

    // ── Canvas mode dispatch ──────────────────────────────────────────────
    fn handle_canvas_interaction(&mut self, cc: &CanvasCtx) {
        puffin::profile_function!();
        let pointer = cc
            .response
            .interact_pointer_pos()
            .or_else(|| cc.ctx.pointer_hover_pos());
        let mode = self.active().mode.clone();
        match mode {
            InteractionMode::Idle => self.active_mut().interact_idle(cc, pointer),
            InteractionMode::Placing { spec } => self.interact_placing(cc, pointer, spec),
            InteractionMode::PlacingTunnel { role } => {
                self.active_mut().interact_placing_tunnel(cc, pointer, role)
            }
            InteractionMode::WireDraw {
                points,
                start_attach,
                cursor,
                dragging,
            } => self
                .active_mut()
                .interact_wire_draw(cc, pointer, points, start_attach, cursor, dragging),
            InteractionMode::SelectionDrag {
                items,
                free_nodes,
                drag_origin,
            } => self
                .active_mut()
                .interact_component_drag(cc, pointer, items, free_nodes, drag_origin),
            InteractionMode::BulkSelect { start, current } => self
                .active_mut()
                .interact_bulk_select(cc, pointer, start, current),
        }
    }

    fn interact_placing(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>, spec: ComponentSpec) {
        if let Some(pos) = pointer {
            let gp = cc.camera.screen_to_grid(pos);
            draw_ghost(cc.painter, &spec, gp, cc.camera, cc.theme);
        }
        if cc.response.clicked() {
            if let Some(pos) = pointer {
                let gp = cc.camera.screen_to_grid(pos);
                self.place_component(spec, gp);
                self.active_mut().mode = InteractionMode::Idle;
            }
        }
    }

}

impl eframe::App for OsmilogApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Frame boundary for every puffin scope recorded this frame (in both
        // logic() and ui() - eframe calls them once each, in that order).
        puffin::GlobalProfiler::lock().new_frame();
        puffin::profile_function!();

        // Installs an async File > Load result if a web load task has delivered
        // one; no-op on native (and every quiet frame on web).
        self.with_io(|io, app| io.poll_pending_load(app));

        self.active_mut().advance_clock(ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            // Exits the process on native; no-op on web (the canvas just stops).
            platform::quit();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        puffin::profile_function!();
        let ctx = ui.ctx().clone();
        let theme = Theme::from_visuals(ui.visuals());

        if self.show_profiler {
            puffin_egui::profiler_window(&ctx);
        }

        self.show_menu_bar(ui, theme);

        // ROM/RAM contents editor windows, drawn while open.
        self.active_mut().show_memory_editors(&ctx);

        // "New Circuit" name dialog, drawn while it's open.
        self.create_new_circuit_dialog(&ctx);

        // Draws the web "Save As" filename modal while it's open (and completes
        // the download on confirm); no-op on native.
        // TODO: Figure out if this weird closure stuff is necessary
        self.with_io(|io, app| io.drive_save_dialog(&ctx, app));

        egui::Panel::left("left_panel")
            .min_size(200.0)
            .resizable(true)
            .show(ui, |ui| {
                // Top half: the component palette (formerly the Add menu).
                // Bottom half: the properties panel for the current selection.
                // The split is a resizable inner top panel; each half scrolls.
                // A min height of half the left panel keeps the palette from
                // shrinking (and re-laying-out the split) as submenus collapse,
                // and leaves less to scroll.
                let half = ui.available_height() * 0.5;
                egui::Panel::top("component_palette")
                    .resizable(true)
                    .min_size(half)
                    .default_size(half)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("palette_scroll")
                            // Don't shrink to content width: fill the panel so
                            // the scrollbar sits at the far right edge.
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                self.show_component_palette(ui);
                            });
                    });
                egui::ScrollArea::vertical()
                    .id_salt("properties_scroll")
                    .show(ui, |ui| {
                        if let Some(action) =
                            crate::gui::properties::show_properties(self.active(), ui)
                        {
                            self.apply_prop_gui_action(action);
                        }
                    });
            });

        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let clip_rect = painter.clip_rect();

        // Update the camera (middle-drag pan, Ctrl+scroll zoom) before drawing so
        // the change applies this same frame.
        self.active_mut().camera.handle_input(&response, &ctx);
        let camera = self.active().camera;

        self.handle_canvas_shortcuts(&ctx);
        self.active().draw(&painter, clip_rect, camera, theme);

        let cc = CanvasCtx {
            response: &response,
            painter: &painter,
            ctx: &ctx,
            camera,
            theme,
        };
        self.handle_canvas_interaction(&cc);
    }
}

// ── Geometry ─────────────────────────────────────────────────────────────────

// Extracts one document's live (non-tombstoned) visual records into the
// index-based entry vectors io.rs persists: PlacedCompKey/PlacedTunnelKey/
// WireNodeKey become positions in the emitted Vecs, so cross-references are
// plain indices rather than ephemeral slotmap keys. Tombstones (kept for undo)
// never reach a file. Also returns the PlacedCompKey -> component-index map, so
// `circuit_entry_of` can emit per-component references (subcircuit links) by
// index.
fn extract_records(
    components: &SlotMap<PlacedCompKey, PlacedComponent>,
    tunnels: &SlotMap<PlacedTunnelKey, PlacedTunnel>,
    wiring: &Wiring,
) -> (CircuitSnapshot, HashMap<PlacedCompKey, usize>) {
    let mut comp_index: HashMap<PlacedCompKey, usize> = HashMap::new();
    let comp_entries: Vec<ComponentEntry> = components
        .iter()
        .filter(|(_, pc)| pc.active)
        .enumerate()
        .map(|(i, (pck, pc))| {
            comp_index.insert(pck, i);
            ComponentEntry {
                spec: pc.spec.clone(),
                grid_pos: pc.grid_pos,
            }
        })
        .collect();

    let mut tunnel_index: HashMap<PlacedTunnelKey, usize> = HashMap::new();
    let tunnel_entries: Vec<TunnelEntry> = tunnels
        .iter()
        .filter(|(_, pt)| pt.active)
        .enumerate()
        .map(|(i, (ptk, pt))| {
            tunnel_index.insert(ptk, i);
            TunnelEntry {
                label: pt.label.clone(),
                role: pt.role,
                grid_pos: pt.grid_pos,
            }
        })
        .collect();

    // WireNodeKey -> position in `nodes`, so segments can reference nodes by
    // index. Built before `segments` reads it. Active segments only reference
    // active nodes, so every SegEntry lookup below resolves.
    let mut node_index: HashMap<crate::gui::wiring::WireNodeKey, usize> = HashMap::new();
    let node_entries: Vec<NodeEntry> = wiring
        .active_nodes()
        .enumerate()
        .map(|(i, (nk, node))| {
            node_index.insert(nk, i);
            let attach = match node.attach {
                NodeAttach::Free => NodeAttachEntry::Free,
                NodeAttach::Pin(pck, pin) => {
                    let (is_input, pin_index) = match pin {
                        PinId::In(InIdx(p)) => (true, p),
                        PinId::Out(OutIdx(p)) => (false, p),
                    };
                    NodeAttachEntry::Pin {
                        comp: comp_index[&pck],
                        is_input,
                        pin_index,
                    }
                }
                NodeAttach::Tunnel(ptk) => NodeAttachEntry::Tunnel {
                    tunnel: tunnel_index[&ptk],
                },
            };
            NodeEntry {
                pos: node.pos,
                attach,
            }
        })
        .collect();

    let seg_entries: Vec<SegEntry> = wiring
        .active_segments()
        .map(|(_, s)| SegEntry {
            a: node_index[&s.a],
            b: node_index[&s.b],
        })
        .collect();

    (
        CircuitSnapshot {
            components: comp_entries,
            tunnels: tunnel_entries,
            nodes: node_entries,
            segments: seg_entries,
        },
        comp_index,
    )
}

pub(crate) fn component_bounding_rect(pc: &PlacedComponent, camera: Camera) -> Rect {
    Rect::from_min_size(
        camera.grid_to_screen(pc.grid_pos),
        pc.shape.size * camera.zoom,
    )
}

// Grid coordinate of a pin: the component's grid_pos plus the anchor's whole-cell
// offset. This is the wiring counterpart of comp_pin_pos (which returns pixels).
pub(crate) fn pin_grid_pos(shape: &ComponentShape, grid_pos: GridPos, pin: PinId) -> GridPos {
    let anchor = match pin {
        PinId::In(InIdx(i)) => &shape.input_anchors[i as usize],
        PinId::Out(OutIdx(i)) => &shape.output_anchors[i as usize],
    };
    GridPos {
        x: grid_pos.x + anchor.cell.x as i32,
        y: grid_pos.y + anchor.cell.y as i32,
    }
}

// Grid coordinate of a tunnel's single pin.
pub(crate) fn tunnel_pin_grid(pt: &PlacedTunnel) -> GridPos {
    let shape = tunnel_shape(pt.role);
    let anchor = match pt.role {
        TunnelRole::Feed => &shape.output_anchors[0],
        TunnelRole::Pull => &shape.input_anchors[0],
    };
    GridPos::new(
        pt.grid_pos.x + anchor.cell.x as i32,
        pt.grid_pos.y + anchor.cell.y as i32,
    )
}

// The screen-space rectangle spanned by a BulkSelect drag's two grid corners,
// normalized so either drag direction yields the same box.
// Takes an already-computed ComponentShape (not &PlacedComponent) so callers
// needing multiple pins from one component compute shape() once and reuse it.
pub(crate) fn comp_pin_pos(
    shape: &ComponentShape,
    grid_pos: GridPos,
    camera: Camera,
    pin: PinId,
) -> Pos2 {
    let tl = camera.grid_to_screen(grid_pos);
    let anchor = match pin {
        PinId::In(InIdx(i)) => &shape.input_anchors[i as usize],
        PinId::Out(OutIdx(i)) => &shape.output_anchors[i as usize],
    };
    // Anchors are whole grid cells from the top-left (itself grid-aligned), so
    // every pin lands exactly on a grid intersection.
    tl + anchor.cell * camera.grid_scale()
}

pub(crate) fn tunnel_bounding_rect(pt: &PlacedTunnel, camera: Camera) -> Rect {
    let size = tunnel_shape(pt.role).size;
    Rect::from_min_size(camera.grid_to_screen(pt.grid_pos), size * camera.zoom)
}

pub(crate) fn tunnel_pin_pos(pt: &PlacedTunnel, camera: Camera) -> Pos2 {
    let shape = tunnel_shape(pt.role);
    let tl = camera.grid_to_screen(pt.grid_pos);
    let anchor = match pt.role {
        TunnelRole::Feed => &shape.output_anchors[0],
        TunnelRole::Pull => &shape.input_anchors[0],
    };
    tl + anchor.cell * camera.grid_scale()
}

pub(crate) fn pin_at_pos<'a>(
    components: impl Iterator<Item = (PlacedCompKey, &'a PlacedComponent)>,
    camera: Camera,
    pos: Pos2,
    kind: PinKind,
) -> Option<(PlacedCompKey, PinId)> {
    puffin::profile_function!();
    let hit_r = camera.scale(PIN_RADIUS * 2.0);
    for (key, pc) in components {
        let shape = &pc.shape;
        match kind {
            PinKind::Output => {
                for i in 0..pc.spec.n_outputs() {
                    let pp = comp_pin_pos(shape, pc.grid_pos, camera, PinId::output(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((key, PinId::output(i as u8)));
                    }
                }
            }
            PinKind::Input => {
                for i in 0..pc.spec.n_inputs() {
                    let pp = comp_pin_pos(shape, pc.grid_pos, camera, PinId::input(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((key, PinId::input(i as u8)));
                    }
                }
            }
        }
    }
    None
}
// A tunnel's single pin under `pos`, regardless of role - a wire can now both
// start and end on either a Feed or a Pull tunnel.
pub(crate) fn tunnel_pin_at_pos<'a>(
    tunnels: impl Iterator<Item = (PlacedTunnelKey, &'a PlacedTunnel)>,
    camera: Camera,
    pos: Pos2,
) -> Option<PlacedTunnelKey> {
    puffin::profile_function!();
    let hit_r = camera.scale(PIN_RADIUS * 2.0);
    for (key, tunnel) in tunnels {
        if tunnel_pin_pos(tunnel, camera).distance(pos) <= hit_r {
            return Some(key);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::wiring::NodeAttach;
    use crate::sim::component::GateOp;

    fn place(app: &mut OsmilogApp, spec: ComponentSpec) -> PlacedCompKey {
        app.place_component(spec, GridPos::new(0, 0))
    }

    // Insert a wire (one segment) between two component pins, positioned at each
    // pin's grid cell, and return the two node keys.
    fn connect_pins(app: &mut OsmilogApp, a: (PlacedCompKey, PinId), b: (PlacedCompKey, PinId)) {
        let pa = pin_grid_pos(
            &app.active().components[a.0].shape,
            app.active().components[a.0].grid_pos,
            a.1,
        );
        let pb = pin_grid_pos(
            &app.active().components[b.0].shape,
            app.active().components[b.0].grid_pos,
            b.1,
        );
        let na = app.active_mut().wiring.nodes.insert(WireNode {
            pos: pa,
            attach: NodeAttach::Pin(a.0, a.1),
            active: true,
        });
        let nb = app.active_mut().wiring.nodes.insert(WireNode {
            pos: pb,
            attach: NodeAttach::Pin(b.0, b.1),
            active: true,
        });
        app.active_mut().wiring.segments.insert(WireSegment {
            a: na,
            b: nb,
            active: true,
        });
    }

    #[test]
    fn test_circuit_file_save_excludes_tombstones() {
        // After a wiring edit leaves tombstones behind, the save file must
        // reflect only the live graph - tombstones never reach disk.
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut app,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        let o = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (a, PinId::output(0)), (g, PinId::input(0)));
        connect_pins(&mut app, (g, PinId::output(0)), (o, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        // Delete the a->g wire; its nodes/segment become tombstones still held
        // in the SlotMaps.
        let seg = app
            .active()
            .wiring
            .active_segments()
            .find(|(_, s)| {
                matches!(app.active().wiring.nodes[s.a].attach, NodeAttach::Pin(k, _) if k == a)
                    || matches!(app.active().wiring.nodes[s.b].attach, NodeAttach::Pin(k, _) if k == a)
            })
            .map(|(k, _)| k)
            .unwrap();
        app.active_mut().delete_wire(seg);
        assert!(app.active().wiring.segments.len() > app.active().wiring.active_segments().count());

        // The saved project carries only live entries, and the reload matches
        // the live graph exactly.
        let file = app.to_project_file();
        let snap = &file.circuits[0].snapshot;
        assert_eq!(
            snap.segments.len(),
            app.active().wiring.active_segments().count()
        );
        assert_eq!(snap.nodes.len(), app.active().wiring.active_nodes().count());

        let json = file.to_json().unwrap();
        let file2 = ProjectFile::from_json(&json).unwrap();
        let mut loaded = OsmilogApp::empty();
        loaded.load_project_file(&file2).unwrap();
        assert_eq!(
            loaded.active().wiring.active_segments().count(),
            app.active().wiring.active_segments().count()
        );
        // A fresh load has no tombstones: every stored entry is live.
        assert_eq!(
            loaded.active().wiring.segments.len(),
            loaded.active().wiring.active_segments().count()
        );
        assert_eq!(
            loaded.active().wiring.nodes.len(),
            loaded.active().wiring.active_nodes().count()
        );
    }

    #[test]
    fn test_load_project_file_clears_undo_history() {
        let mut app = OsmilogApp::empty();
        place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        place(&mut app, ComponentSpec::Output);
        let file = app.to_project_file();

        let mut loaded = OsmilogApp::empty();
        loaded.load_project_file(&file).unwrap();

        assert!(!loaded.active().history.can_undo());
        assert!(!loaded.active().history.can_redo());
    }

    #[test]
    fn test_load_project_file_rejects_bad_component_index() {
        let entry = CircuitEntry {
            name: "Main".to_string(),
            snapshot: CircuitSnapshot {
                components: vec![ComponentEntry {
                    spec: ComponentSpec::Output,
                    grid_pos: GridPos::ZERO,
                }],
                tunnels: vec![],
                nodes: vec![NodeEntry {
                    pos: GridPos::ZERO,
                    attach: NodeAttachEntry::Pin {
                        comp: 5,
                        is_input: true,
                        pin_index: 0,
                    },
                }],
                segments: vec![],
            },
            subcircuits: vec![],
        };
        let file = ProjectFile::new(0, vec![entry]);

        let mut app = OsmilogApp::empty();
        let before = app.active().components.len();
        assert!(app.load_project_file(&file).is_err());
        // A rejected file must not leave the app half-overwritten.
        assert_eq!(app.active().components.len(), before);
    }

    // (Unsupported-version rejection is a project-file concern - see
    // test_project_file_validate_rejects_bad_files.)

    #[test]
    fn test_copy_single_component_then_paste_creates_offset_copy() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        let original = app.active().components[a].grid_pos;

        app.active_mut().selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();
        assert!(!app.clipboard.is_empty());

        app.paste_clipboard();

        assert_eq!(app.active().active_components().count(), 2);
        let pasted = app
            .active()
            .active_components()
            .find(|(k, _)| *k != a)
            .map(|(k, _)| k)
            .unwrap();
        assert_eq!(
            app.active().components[pasted].grid_pos,
            GridPos::new(original.x + 2, original.y + 2)
        );
        assert_eq!(
            app.active().selected,
            Some(Selection::Single(Selected::Component(pasted)))
        );
    }

    #[test]
    fn test_paste_after_undo_of_original_still_works() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        app.active_mut().selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();

        // Undo the original placement: it's now tombstoned.
        app.active_mut().undo();
        assert!(!app.active().components[a].active);

        app.paste_clipboard();

        // The paste is independent of the now-tombstoned original.
        let pasted = app
            .active()
            .active_components()
            .find(|(k, _)| *k != a)
            .map(|(k, _)| k);
        assert!(pasted.is_some());
        assert_eq!(app.active().active_components().count(), 1);
    }

    #[test]
    fn test_paste_after_editing_original_is_unaffected() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        let original_pos = app.active().components[a].grid_pos;
        app.active_mut().selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();

        // Move the original after copying.
        app.active_mut().components[a].grid_pos = GridPos::new(100, 100);

        app.paste_clipboard();

        // The pasted copy reflects the pre-edit snapshot, offset from the
        // original's position at copy time - not its current position.
        let pasted = app
            .active()
            .active_components()
            .find(|(k, _)| *k != a)
            .map(|(k, _)| k)
            .unwrap();
        assert_eq!(
            app.active().components[pasted].grid_pos,
            GridPos::new(original_pos.x + 2, original_pos.y + 2)
        );
    }

    #[test]
    fn test_paste_normalizes_selection_to_single_for_one_item() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        app.active_mut().selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();
        app.paste_clipboard();
        assert!(matches!(app.active().selected, Some(Selection::Single(_))));
    }

    #[test]
    fn test_paste_is_one_undo_batch() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let b = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (a, PinId::output(0)), (b, PinId::input(0)));
        app.active_mut().rebuild_circuit();
        let seg = app.active().wiring.active_segments().next().unwrap().0;

        app.active_mut().selected = Some(Selection::Bulk(vec![
            Selected::Component(a),
            Selected::Component(b),
            Selected::Wire(seg),
        ]));
        app.copy_selection();
        app.paste_clipboard();
        assert_eq!(app.active().active_components().count(), 4);
        assert_eq!(app.active().wiring.active_segments().count(), 2);

        // One undo removes the entire pasted batch (components + wiring).
        app.active_mut().undo();
        assert_eq!(app.active().active_components().count(), 2);
        assert_eq!(app.active().wiring.active_segments().count(), 1);
    }

    #[test]
    fn test_paste_noop_when_clipboard_empty() {
        let mut app = OsmilogApp::empty();
        place(&mut app, ComponentSpec::Output);
        assert!(app.clipboard.is_empty());

        let before = app.active().components.len();
        app.paste_clipboard();
        assert_eq!(app.active().components.len(), before);
        assert_eq!(app.active().selected, None);
    }

    // ── Multiple circuits ──────────────────────────────────────────────────

    #[test]
    fn empty_app_has_one_active_main_document() {
        let app = OsmilogApp::empty();
        assert_eq!(app.documents.len(), 1);
        assert_eq!(app.doc_order.len(), 1);
        assert_eq!(app.doc_order[0], app.active_id);
        assert_eq!(app.documents[app.active_id].name, "Main");
    }

    #[test]
    fn create_circuit_doc_adds_active_blank_document() {
        let mut app = OsmilogApp::empty();
        let main = app.active_id;
        place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        assert_eq!(app.active().components.len(), 1);

        app.create_circuit_doc("C2".to_string());

        // A second document exists and is now active, with a blank canvas.
        assert_eq!(app.documents.len(), 2);
        assert_eq!(app.doc_order.len(), 2);
        assert_ne!(app.active_id, main);
        assert_eq!(app.documents[app.active_id].name, "C2");
        assert!(app.active().components.is_empty());
        // The previous document's records are untouched, still reachable by key.
        assert_eq!(app.documents[main].state.components.len(), 1);
    }

    #[test]
    fn switching_circuits_parks_and_restores_state() {
        let mut app = OsmilogApp::empty();
        let main = app.active_id;

        // Build a settled AND-of-two-highs -> Output on "Main".
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let b = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut app,
            ComponentSpec::Gate(Gate {
                op: GateOp::And,
                n_inputs: 2,
                width: 1,
            }),
        );
        let o = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (a, PinId::output(0)), (g, PinId::input(0)));
        connect_pins(&mut app, (b, PinId::output(0)), (g, PinId::input(1)));
        connect_pins(&mut app, (g, PinId::output(0)), (o, PinId::input(0)));
        app.active_mut().rebuild_circuit();
        let o_key = app.active().components[o].key;
        assert_eq!(app.active().circuit.read_output(o_key), Value::ONE);

        // Create + switch to a blank second circuit: Main's contents vanish
        // from the live fields.
        app.create_circuit_doc("C2".to_string());
        assert!(app.active().components.is_empty());

        // Switch back: Main's components and settled net values return intact,
        // without a rebuild (the parked circuit kept its nets).
        app.switch_circuit(main);
        assert_eq!(app.active_id, main);
        assert_eq!(app.active().components.len(), 4);
        assert_eq!(app.active().circuit.read_output(o_key), Value::ONE);
    }

    #[test]
    fn switch_to_active_is_a_noop() {
        let mut app = OsmilogApp::empty();
        let main = app.active_id;
        place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));

        app.switch_circuit(main);

        assert_eq!(app.active_id, main);
        assert_eq!(app.documents.len(), 1);
        assert_eq!(app.active().components.len(), 1);
    }

    #[test]
    fn subcircuit_placement_derives_interface_and_simulates() {
        let mut app = OsmilogApp::empty();
        let main = app.active_id;

        // Main: a 1-bit passthrough Input -> Output (one boundary pin each).
        let in_main = place(&mut app, ComponentSpec::Input(Input { bits: 0, width: 1 }));
        let out_main = place(&mut app, ComponentSpec::Output);
        connect_pins(
            &mut app,
            (in_main, PinId::output(0)),
            (out_main, PinId::input(0)),
        );
        app.active_mut().rebuild_circuit();

        // New circuit C2 (now active, Main parked); place Main as a subcircuit.
        app.create_circuit_doc("C2".to_string());
        let spec = app.subcircuit_spec(main);
        let sub = app.place_component(spec, GridPos::new(5, 5));

        // Interface derived from Main's one Input and one Output.
        assert_eq!(app.active().components[sub].spec.n_inputs(), 1);
        assert_eq!(app.active().components[sub].spec.n_outputs(), 1);

        // Drive it end-to-end: C2 Input(=1) -> sub -> C2 Output. The passthrough
        // subcircuit settles a 1 out through the boundary.
        let x = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let y = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (x, PinId::output(0)), (sub, PinId::input(0)));
        connect_pins(&mut app, (sub, PinId::output(0)), (y, PinId::input(0)));
        app.active_mut().rebuild_circuit();
        let y_key = app.active().components[y].key;
        assert_eq!(app.active().circuit.read_output(y_key), Value::ONE);
    }

    #[test]
    fn subcircuit_placement_prevents_cycles() {
        let mut app = OsmilogApp::empty();
        let main = app.active_id;

        // Main is a passthrough so it has a usable boundary.
        let in_main = place(&mut app, ComponentSpec::Input(Input { bits: 0, width: 1 }));
        let out_main = place(&mut app, ComponentSpec::Output);
        connect_pins(
            &mut app,
            (in_main, PinId::output(0)),
            (out_main, PinId::input(0)),
        );
        app.active_mut().rebuild_circuit();

        // C2 contains a subcircuit of Main.
        app.create_circuit_doc("C2".to_string());
        let c2 = app.active_id;
        let spec = app.subcircuit_spec(main);
        app.place_component(spec, GridPos::new(5, 5));

        // Back on Main: placing C2 here would form Main -> C2 -> Main. Rejected.
        app.switch_circuit(main);
        assert!(
            app.would_cycle(c2),
            "C2 references Main, so nesting it cycles"
        );
        assert!(app.would_cycle(main), "a circuit can't contain itself");

        // A fresh, unrelated circuit is fine to nest.
        app.create_circuit_doc("C3".to_string());
        let c3 = app.active_id;
        app.switch_circuit(main);
        assert!(!app.would_cycle(c3));
    }

    // ── ProjectFile (multi-circuit) save/load ───────────────────────────────

    #[test]
    fn test_project_file_round_trip_multiple_circuits() {
        let mut app = OsmilogApp::empty();

        // Main: Input(1) -> NOT -> Output, which settles a 0.
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let n = place(
            &mut app,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        let o = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (a, PinId::output(0)), (n, PinId::input(0)));
        connect_pins(&mut app, (n, PinId::output(0)), (o, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        // C2 (now active): Input(1) -> Output, a passthrough settling a 1.
        app.create_circuit_doc("C2".to_string());
        let x = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let y = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (x, PinId::output(0)), (y, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        // Save the whole workspace (C2 is the active document) and reload it.
        let file = app.to_project_file();
        assert_eq!(file.circuits.len(), 2);
        assert_eq!(file.active, 1); // C2 is second in doc_order and active.
        let json = file.to_json().unwrap();
        let file2 = ProjectFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_project_file(&file2).unwrap();

        // Both documents restored, in order, with C2 active.
        let names: Vec<String> = loaded
            .doc_order
            .iter()
            .map(|&d| loaded.documents[d].name.clone())
            .collect();
        assert_eq!(names, vec!["Main".to_string(), "C2".to_string()]);
        assert_eq!(loaded.active_id, loaded.doc_order[1]);

        // The active document (C2) simulates: its Output reads 1.
        let c2_out = loaded
            .active()
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.active().circuit.read_output(c2_out), Value::ONE);

        // Switching to Main brings its (independent) state back: Output reads 0.
        let main = loaded.doc_order[0];
        loaded.switch_circuit(main);
        let main_out = loaded
            .active()
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.active().circuit.read_output(main_out), Value::ZERO);
    }

    #[test]
    fn test_load_project_file_upgrades_legacy_v2() {
        // A hand-built v2 single-circuit file: Input(1) -> Output. (The
        // upgrade itself - LegacyV2File -> one-circuit ProjectFile - is
        // covered in crate::io's own tests; this checks that OsmilogApp loads
        // the upgraded project and simulates it correctly.)
        let v2 = crate::io::LegacyV2File {
            version: crate::io::LEGACY_SINGLE_CIRCUIT_VERSION,
            snapshot: CircuitSnapshot {
                components: vec![
                    ComponentEntry {
                        spec: ComponentSpec::Input(Input { bits: 1, width: 1 }),
                        grid_pos: GridPos::new(0, 0),
                    },
                    ComponentEntry {
                        spec: ComponentSpec::Output,
                        grid_pos: GridPos::new(5, 0),
                    },
                ],
                tunnels: vec![],
                nodes: vec![
                    NodeEntry {
                        pos: GridPos::new(0, 0),
                        attach: NodeAttachEntry::Pin {
                            comp: 0,
                            is_input: false,
                            pin_index: 0,
                        },
                    },
                    NodeEntry {
                        pos: GridPos::new(5, 0),
                        attach: NodeAttachEntry::Pin {
                            comp: 1,
                            is_input: true,
                            pin_index: 0,
                        },
                    },
                ],
                segments: vec![SegEntry { a: 0, b: 1 }],
            },
        };
        let json = serde_json::to_string(&v2).unwrap();
        let project = ProjectFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_project_file(&project).unwrap();
        assert_eq!(loaded.documents.len(), 1);
        let out = loaded
            .active()
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.active().circuit.read_output(out), Value::ONE);
    }

    #[test]
    fn test_project_file_subcircuit_round_trip() {
        let mut app = OsmilogApp::empty();
        let main = app.active_id;

        // Main: Input(1) -> Output, a passthrough.
        let in_main = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let out_main = place(&mut app, ComponentSpec::Output);
        connect_pins(
            &mut app,
            (in_main, PinId::output(0)),
            (out_main, PinId::input(0)),
        );
        app.active_mut().rebuild_circuit();

        // C2: Input(1) -> [Main as subcircuit] -> Output.
        app.create_circuit_doc("C2".to_string());
        let spec = app.subcircuit_spec(main);
        let sub = app.place_component(spec, GridPos::new(5, 5));
        let x = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let y = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (x, PinId::output(0)), (sub, PinId::input(0)));
        connect_pins(&mut app, (sub, PinId::output(0)), (y, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        // The subcircuit reference is emitted as an index into `circuits`.
        let file = app.to_project_file();
        let c2 = &file.circuits[file.active];
        assert_eq!(c2.subcircuits.len(), 1);
        assert_eq!(c2.subcircuits[0].circuit, 0); // Main is circuit 0.

        let json = file.to_json().unwrap();
        let file2 = ProjectFile::from_json(&json).unwrap();

        // After reload (C2 active), the subcircuit rebinds to the reloaded Main
        // and the whole thing still settles a 1 through the boundary.
        let mut loaded = OsmilogApp::empty();
        loaded.load_project_file(&file2).unwrap();
        let sub_reloaded = loaded
            .active()
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Subcircuit { .. }))
            .expect("subcircuit component restored");
        assert_eq!(sub_reloaded.spec.n_inputs(), 1);
        assert_eq!(sub_reloaded.spec.n_outputs(), 1);
        let y_key = loaded
            .active()
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.active().circuit.read_output(y_key), Value::ONE);
    }

    #[test]
    fn test_project_file_subcircuit_forward_reference_round_trip() {
        // The referencing circuit (Main, index 0) refers to a *later*-indexed
        // circuit (C2, index 1), so on load Main is populated while C2 is still
        // blank. The placed subcircuit must still get its cached pin arity (not
        // a 0-pin placeholder) or wiring to it panics.
        let mut app = OsmilogApp::empty();
        let main = app.active_id;

        // C2: Input(1) -> Output passthrough.
        app.create_circuit_doc("C2".to_string());
        let c2 = app.active_id;
        let c2_in = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let c2_out = place(&mut app, ComponentSpec::Output);
        connect_pins(
            &mut app,
            (c2_in, PinId::output(0)),
            (c2_out, PinId::input(0)),
        );
        app.active_mut().rebuild_circuit();

        // Back on Main: Input(1) -> [C2 as subcircuit] -> Output.
        app.switch_circuit(main);
        let spec = app.subcircuit_spec(c2);
        let sub = app.place_component(spec, GridPos::new(5, 5));
        let x = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let y = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (x, PinId::output(0)), (sub, PinId::input(0)));
        connect_pins(&mut app, (sub, PinId::output(0)), (y, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        let file = app.to_project_file();
        assert_eq!(file.active, 0); // Main active.
        assert_eq!(file.circuits[0].subcircuits[0].circuit, 1); // refers to C2.

        let json = file.to_json().unwrap();
        let file2 = ProjectFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_project_file(&file2).unwrap();
        let y_key = loaded
            .active()
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.active().circuit.read_output(y_key), Value::ONE);
    }
}

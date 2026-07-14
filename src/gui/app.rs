use eframe;
use egui::{Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind};
use slotmap::{new_key_type, SecondaryMap, SlotMap};
use std::collections::HashMap;

use crate::gui::canvas_draw::{
    draw_component, draw_ghost, draw_grid, draw_reticle, draw_tunnel, draw_tunnel_ghost,
    extend_segment, value_stroke,
};
use crate::gui::clipboard::Clipboard;
use crate::gui::clock::ClockRun;
use crate::gui::document::{default_new_circuit_name, CircuitDoc, DocId, Document};
use crate::gui::geometry::{tunnel_shape, Camera, GridPos, ZOOM_SCROLL_SPEED};
use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::history::History;
use crate::gui::memory_editor::MemKind;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::shape::ComponentShape;
use crate::gui::theme::Theme;
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

// ── CanvasCtx ─────────────────────────────────────────────────────────────────

// Ambient egui/render handles used by the canvas-interaction dispatch and its
// per-mode methods. Built fresh each frame in `ui()`, never stored on
// OsmilogApp - just a bundle of the 5 values `handle_canvas_interaction` and
// its 6 `interact_*` methods would otherwise all repeat individually.
struct CanvasCtx<'a> {
    response: &'a egui::Response,
    painter: &'a Painter,
    ctx: &'a egui::Context,
    camera: Camera,
    theme: Theme,
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
    fn switch_circuit(&mut self, target: DocId) {
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
        self.active().clock.run != ClockRun::Stopped
    }

    // True while live *value* edits must be blocked - an Input's bits, a ROM's
    // or RAM's contents. Unlike the blanket editing_locked(), these are carved
    // out of the lock while Paused: a paused run is frozen structurally, but an
    // Input can still be driven to new stimulus and memory poked between steps,
    // so this is true only while actively Playing. Structural edits (widths,
    // wiring, add/delete) stay gated on editing_locked().
    pub fn value_editing_locked(&self) -> bool {
        self.active().clock.run == ClockRun::Playing
    }

    // Builds the live sim Component (needs the document registry, so this part
    // can't live on Document - see instantiate below) then hands it to
    // Document::place_component for the per-document record/undo bookkeeping.
    fn place_component(&mut self, spec: ComponentSpec, grid_pos: GridPos) -> PlacedCompKey {
        let comp = self.instantiate(&spec);
        self.active_mut().place_component(comp, spec, grid_pos)
    }

    // ── Subcircuits ───────────────────────────────────────────────────────────
    //
    // Builds a live sim Component from a ComponentSpec. Identical to
    // spec.to_component() for every primitive type; a Subcircuit spec instead
    // has its inner Circuit built from the referenced document (which
    // to_component can't do - it has no document registry). This is the one
    // spec->component build path the GUI uses (place_component /
    // reconfigure_component), so subcircuits always get a real inner circuit.
    fn instantiate(&self, spec: &ComponentSpec) -> Component {
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

    /// Shows the properties panel for the selected item. Edits call
    /// `self.reconfigure_component()` with an updated `ComponentSpec`.
    fn show_properties(&mut self, ui: &mut egui::Ui) {
        let sel = match &self.active().selected {
            None => {
                ui.label("Click a component or tunnel to select it.");
                return;
            }
            Some(Selection::Bulk(items)) => {
                ui.heading("SELECTION");
                ui.separator();
                ui.label(format!("{} items selected.", items.len()));
                ui.label("Press Backspace or Delete to remove them.");
                return;
            }
            Some(Selection::Single(sel)) => *sel,
        };
        // A run session makes structural edits read-only, but value edits
        // (an Input's bits, ROM/RAM contents) stay live while Paused. Rather
        // than blanket-disabling the panel, each per-component editor gates its
        // own widgets - structural ones on editing_locked(), value ones on
        // value_editing_locked() - so the carve-out lands on exactly those.
        match sel {
            Selected::Component(key) => self.show_component_properties(key, ui),
            Selected::Tunnel(key) => self.show_tunnel_properties(key, ui),
            Selected::Wire(_) => {
                ui.heading("WIRE");
                ui.label("A wire segment. Press Backspace or Delete to remove it.");
            }
        }

        ui.separator();
        // Delete is structural: disabled for the whole run session.
        ui.add_enabled_ui(!self.editing_locked(), |ui| {
            if ui.button("Delete").clicked() {
                match sel {
                    Selected::Component(key) => self.active_mut().delete_component(key),
                    Selected::Tunnel(key) => self.active_mut().delete_tunnel(key),
                    Selected::Wire(seg) => self.active_mut().delete_wire(seg),
                }
            }
        });
    }

    fn show_tunnel_properties(&mut self, key: PlacedTunnelKey, ui: &mut egui::Ui) {
        let role = self.active().tunnels[key].role;
        let tunnel_key = self.active().tunnels[key].key;

        ui.heading(match role {
            TunnelRole::Feed => "TUNNEL (FEED)",
            TunnelRole::Pull => "TUNNEL (PULL)",
        });
        ui.separator();
        // A tunnel's label is structural (it rewires nets): read-only for the
        // whole run session.
        ui.add_enabled_ui(!self.editing_locked(), |ui| {
            ui.label("Label:");
            let mut label = self.active().tunnels[key].label.clone();
            let response = ui.text_edit_singleline(&mut label);
            if response.changed() {
                self.active_mut().tunnels[key].label = label.clone();
            }

            // Commit on any focus loss (Enter/Tab/click-away), not just Enter:
            // the record label is already updated live above (on `changed()`),
            // but the circuit's hasn't committed yet, so read the old label
            // from the circuit to both detect a real change and capture undo's
            // restore value. (rebuild_circuit also reconciles as a backstop.)
            if response.lost_focus() {
                let old_label = self
                    .active()
                    .circuit
                    .tunnels
                    .get(tunnel_key)
                    .map(|t| t.label.clone());
                if old_label.as_deref() != Some(label.as_str()) {
                    let doc = self.active_mut();
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
            }
        });
    }

    // Draws "<label> [DragValue]" in a horizontal row and returns whether the
    // value changed - the recurring widget idiom nearly every ComponentSpec
    // arm below uses for one numeric parameter. Generic over the DragValue's
    // numeric type since fields vary between u8/u32/usize.
    fn labeled_drag<Num: egui::emath::Numeric>(
        ui: &mut egui::Ui,
        label: &str,
        value: &mut Num,
        range: std::ops::RangeInclusive<Num>,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(label);
            changed = ui.add(egui::DragValue::new(value).range(range)).changed();
        });
        changed
    }

    // Draws the "bits" widget shared by Input and Constant: a checkbox when
    // width == 1 (a single bit reads as on/off), else a labeled DragValue
    // clamped to the value's width. Returns whether it changed. Callers
    // choose which enable-gate wraps this (Input gates it on value_ok,
    // Constant on structural_ok - see each arm).
    fn bits_widget(ui: &mut egui::Ui, bits: &mut u32, width: u8) -> bool {
        if width == 1 {
            let mut high = *bits != 0;
            if ui.checkbox(&mut high, "Toggle").clicked() {
                *bits = high as u32;
                return true;
            }
            false
        } else {
            Self::labeled_drag(ui, "Bits:", bits, 0..=Value::mask(width))
        }
    }

    fn show_component_properties(&mut self, key: PlacedCompKey, ui: &mut egui::Ui) {
        let comp_key = self.active().components[key].key;

        ui.heading(self.active().components[key].spec.label());
        ui.separator();

        // A run session locks *structural* edits (widths, arity, wiring) for its
        // whole duration, but carves out live *value* edits - an Input's bits and
        // ROM/RAM contents - which stay pokeable while Paused (blocked only while
        // actively Playing). Every editable widget below is gated on whichever
        // predicate applies; read-only value displays stay ungated so a running
        // circuit's state remains observable.
        let structural_ok = !self.editing_locked();
        let value_ok = !self.value_editing_locked();

        // The spec is matched *by reference*: a ROM/RAM spec carries its whole
        // contents buffer (up to tens of MiB), so cloning it every frame just to
        // own the match scrutinee is out. Borrowing it means the arms can't call
        // the &mut self mutators (reconfigure/switch/open-editor) while the match
        // is live, so each records a deferred PropEdit that we apply once the
        // borrow ends, just past the match.
        enum PropEdit {
            Reconfigure(ComponentSpec),
            OpenRom,
            OpenRam,
            OpenCircuit(DocId),
        }
        let mut edit: Option<PropEdit> = None;

        let fmt_val = |v: Value| match v {
            Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
            Value::Floating => "Floating".to_string(),
            Value::Invalid => "Invalid (width mismatch)".to_string(),
        };

        match &self.active().components[key].spec {
            ComponentSpec::Input(Input {
                mut bits,
                mut width,
            }) => {
                let mut changed = false;
                ui.label(format!("Value: 0x{:X}", bits));
                // `bits` is the live value: editable while Paused.
                ui.add_enabled_ui(value_ok, |ui| {
                    changed |= Self::bits_widget(ui, &mut bits, width);
                });
                // `width` is structural: locked for the whole run session.
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Width:", &mut width, 1..=32);
                });
                if changed {
                    bits &= Value::mask(width); // In case width was changed below max `bits` value
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Input(Input {
                        bits,
                        width,
                    })));
                }
            }
            ComponentSpec::Constant(Constant {
                mut bits,
                mut width,
            }) => {
                let mut changed = false;
                ui.label(format!("Value: 0x{:X}", bits));
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::bits_widget(ui, &mut bits, width);
                    changed |= Self::labeled_drag(ui, "Width:", &mut width, 1..=32);
                });
                if changed {
                    bits &= Value::mask(width); // In case width was changed below max `bits` value
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Constant(Constant {
                        bits,
                        width,
                    })));
                }
            }
            ComponentSpec::Output => {
                let val = self.active().circuit.read_output(comp_key);
                ui.label(format!("Value: {}", fmt_val(val)));
            }
            ComponentSpec::Gate(Gate {
                op,
                mut n_inputs,
                mut width,
            }) => {
                let op = *op;
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    if op != GateOp::Not {
                        changed |= Self::labeled_drag(ui, "Inputs:", &mut n_inputs, 2..=8);
                    }
                    changed |= Self::labeled_drag(ui, "Width:", &mut width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Gate(Gate {
                        op,
                        n_inputs,
                        width,
                    })));
                }
            }
            ComponentSpec::Mux(Mux {
                mut data_width,
                mut sel_width,
            }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                    changed |= Self::labeled_drag(ui, "Sel width:", &mut sel_width, 1..=4);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Mux(Mux {
                        data_width,
                        sel_width,
                    })));
                }
            }
            ComponentSpec::Demux(Demux {
                mut data_width,
                mut sel_width,
            }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                    changed |= Self::labeled_drag(ui, "Sel width:", &mut sel_width, 1..=4);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Demux(Demux {
                        data_width,
                        sel_width,
                    })));
                }
            }
            ComponentSpec::Reg(RegConf { mut data_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Reg(RegConf {
                        data_width,
                    })));
                }

                let cur = self.active().circuit.components[comp_key].pins.out_cache[0];
                ui.label(format!("Value: {}", fmt_val(cur)));
            }
            ComponentSpec::ShiftReg(ShiftRegConf {
                mut data_width,
                mut num_stages,
                mut parallel_load,
            }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                    changed |= Self::labeled_drag(ui, "Stages:", &mut num_stages, 1..=16);
                    ui.horizontal(|ui| {
                        changed |= ui.checkbox(&mut parallel_load, "Parallel load").changed();
                    });
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::ShiftReg(
                        ShiftRegConf {
                            data_width,
                            num_stages,
                            parallel_load,
                        },
                    )));
                }

                for (i, v) in self.active().circuit.components[comp_key]
                    .pins
                    .out_cache
                    .iter()
                    .enumerate()
                {
                    ui.label(format!("Stage {i}: {}", fmt_val(*v)));
                }
            }
            ComponentSpec::Counter(CounterConf {
                mut data_width,
                mut max_value,
                mut overflow_action,
            }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                    changed |= Self::labeled_drag(
                        ui,
                        "Max value:",
                        &mut max_value,
                        0..=Value::mask(data_width),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Overflow action:");
                        egui::ComboBox::from_id_salt(key)
                            .selected_text(format!("{overflow_action:?}"))
                            .show_ui(ui, |ui| {
                                for action in [
                                    OverflowAction::Wrap,
                                    OverflowAction::StayMax,
                                    OverflowAction::PassMax,
                                    OverflowAction::LoadNext,
                                ] {
                                    changed |= ui
                                        .selectable_value(
                                            &mut overflow_action,
                                            action,
                                            format!("{action:?}"),
                                        )
                                        .changed();
                                }
                            });
                    });
                });
                if changed {
                    max_value = max_value.min(Value::mask(data_width)); // Re-cap in case data_width shrank below max_value
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Counter(CounterConf {
                        data_width,
                        max_value,
                        overflow_action,
                    })));
                }

                let q = self.active().circuit.components[comp_key].pins.out_cache[0];
                let carry = self.active().circuit.components[comp_key].pins.out_cache[1];
                ui.label(format!("Q: {}", fmt_val(q)));
                ui.label(format!("Carry: {}", fmt_val(carry)));
            }
            ComponentSpec::DFlipFlop(_)
            | ComponentSpec::TFlipFlop(_)
            | ComponentSpec::JKFlipFlop(_)
            | ComponentSpec::SRFlipFlop(_) => {
                let cur = self.active().circuit.components[comp_key].pins.out_cache[0];
                ui.label(format!("Value: {}", fmt_val(cur)));
            }
            ComponentSpec::Encoder(Encoder { mut sel_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Sel width:", &mut sel_width, 0..=4);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Encoder(Encoder {
                        sel_width,
                    })));
                }
            }
            ComponentSpec::Adder(Adder { mut data_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Adder(Adder {
                        data_width,
                    })));
                }
            }
            ComponentSpec::Subtractor(Subtractor { mut data_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Subtractor(
                        Subtractor { data_width },
                    )));
                }
            }
            ComponentSpec::Multiplier(Multiplier { mut data_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Multiplier(
                        Multiplier { data_width },
                    )));
                }
            }
            ComponentSpec::Divider(Divider { mut data_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Divider(Divider {
                        data_width,
                    })));
                }
            }
            ComponentSpec::Comparator(Comparator { mut data_width }) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Comparator(
                        Comparator { data_width },
                    )));
                }
            }
            // A ROM's contents buffer is huge, so its spec is matched by
            // reference here (never cloned per-frame) - the whole reason the spec
            // match above borrows rather than owns. Widths are structural;
            // rom.resized() preserve-and-fits the contents into a fresh owned
            // buffer, and editing the contents is a value edit (live while Paused).
            ComponentSpec::Rom(
                rom @ Rom {
                    mut data_width,
                    mut address_width,
                    ..
                },
            ) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                    changed |= Self::labeled_drag(
                        ui,
                        "Address width:",
                        &mut address_width,
                        1..=MAX_ADDRESS_WIDTH,
                    );
                    ui.label(format!("{} words", 1usize << address_width));
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Rom(
                        rom.resized(data_width, address_width),
                    )));
                }
                ui.add_enabled_ui(value_ok, |ui| {
                    if ui.button("Edit contents…").clicked() {
                        edit = Some(PropEdit::OpenRom);
                    }
                });
            }
            // Same reasoning as Rom, above (huge contents buffer, matched by
            // reference); read behavior joins the widths as structural.
            ComponentSpec::Ram(
                ram @ Ram {
                    mut data_width,
                    mut address_width,
                    mut read_behavior,
                    ..
                },
            ) => {
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    changed |= Self::labeled_drag(ui, "Data width:", &mut data_width, 1..=32);
                    changed |= Self::labeled_drag(
                        ui,
                        "Address width:",
                        &mut address_width,
                        1..=MAX_ADDRESS_WIDTH,
                    );
                    ui.label(format!("{} words", 1usize << address_width));
                    ui.horizontal(|ui| {
                        ui.label("Read behavior:");
                        egui::ComboBox::from_id_salt(key)
                            .selected_text(format!("{read_behavior:?}"))
                            .show_ui(ui, |ui| {
                                for rb in
                                    [ReadBehavior::ReadAfterWrite, ReadBehavior::WriteAfterRead]
                                {
                                    changed |= ui
                                        .selectable_value(&mut read_behavior, rb, format!("{rb:?}"))
                                        .changed();
                                }
                            });
                    });
                });
                if changed {
                    let mut resized = ram.resized(data_width, address_width);
                    resized.read_behavior = read_behavior;
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Ram(resized)));
                }
                ui.add_enabled_ui(value_ok, |ui| {
                    if ui.button("Edit contents…").clicked() {
                        edit = Some(PropEdit::OpenRam);
                    }
                });

                let cur = self.active().circuit.components[comp_key].pins.out_cache[0];
                ui.label(format!("DO: {}", fmt_val(cur)));
            }
            ComponentSpec::Splitter {
                mut width,
                arm_bits,
                mut direction,
            } => {
                // let mut width = *width;
                let mut arm_bits = arm_bits.clone();
                let mut changed = false;
                ui.add_enabled_ui(structural_ok, |ui| {
                    let before_dir = direction;
                    ui.horizontal(|ui| {
                        ui.label("Fan Direction:");
                        ui.selectable_value(&mut direction, FanDirection::Right, "Split");
                        ui.selectable_value(&mut direction, FanDirection::Left, "Combine");
                    });
                    changed |= direction != before_dir;

                    changed |= Self::labeled_drag(ui, "Data width:", &mut width, 1..=32);
                    let mut arms = arm_bits.len() as u8;
                    changed |= Self::labeled_drag(ui, "Arms:", &mut arms, 1..=16);

                    // Apply width/arms bookkeeping before rendering bit rows below,
                    // so a shrink is reflected the same frame; truncating drops
                    // any bits assigned to a removed arm.
                    arm_bits.resize_with(arms as usize, Vec::new);
                    for list in &mut arm_bits {
                        list.retain(|&b| b < width);
                    }

                    for bit in 0..width {
                        let mut current_arm = arm_bits
                            .iter()
                            .position(|list| list.contains(&bit))
                            .map(|i| i as u8);
                        let before = current_arm;
                        ui.horizontal(|ui| {
                            ui.label(format!("Bit {bit}:"));
                            egui::ComboBox::from_id_salt((key, bit))
                                .selected_text(match current_arm {
                                    Some(a) => format!("Arm {a}"),
                                    None => "None".to_string(),
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut current_arm, None, "None");
                                    for a in 0..arms {
                                        ui.selectable_value(
                                            &mut current_arm,
                                            Some(a),
                                            format!("Arm {a}"),
                                        );
                                    }
                                });
                        });
                        if current_arm != before {
                            for list in &mut arm_bits {
                                list.retain(|&b| b != bit);
                            }
                            if let Some(a) = current_arm {
                                arm_bits[a as usize].push(bit);
                            }
                            changed = true;
                        }
                    }
                });
                if changed {
                    edit = Some(PropEdit::Reconfigure(ComponentSpec::Splitter {
                        width,
                        arm_bits,
                        direction,
                    }));
                }
            }
            // Read-only: a subcircuit's interface is derived from the referenced
            // document, not edited here. Offer a jump to edit that document
            // (mirrors ROM's "Edit contents…" affordance); interface changes
            // are picked up on switch-back (refresh_subcircuits).
            ComponentSpec::Subcircuit {
                doc,
                name,
                input_widths,
                output_widths,
            } => {
                let doc = *doc;
                ui.label(format!("Circuit: {name}"));
                ui.label(format!(
                    "{} input(s), {} output(s)",
                    input_widths.len(),
                    output_widths.len()
                ));
                // Navigating into the child circuit is a structural action
                // (it switches the active document): locked during a run.
                ui.add_enabled_ui(structural_ok, |ui| {
                    if ui.button("Open circuit").clicked() {
                        edit = Some(PropEdit::OpenCircuit(doc));
                    }
                });
            }
        }

        match edit {
            Some(PropEdit::Reconfigure(spec)) => self.reconfigure_component(key, spec),
            Some(PropEdit::OpenRom) => self.active_mut().memory_editor.open(key, MemKind::Rom),
            Some(PropEdit::OpenRam) => self.active_mut().memory_editor.open(key, MemKind::Ram),
            Some(PropEdit::OpenCircuit(doc)) => self.switch_circuit(doc),
            None => {}
        }
    }

    // Swaps a placed component's parameters. PlacedCompKey stays stable, so
    // attached wires survive - we only drop wire nodes for pins the new
    // arity no longer has, re-sync the rest, then rebuild.
    fn reconfigure_component(&mut self, pc_key: PlacedCompKey, new_spec: ComponentSpec) {
        let old_key = self.active().components[pc_key].key;
        let grid_pos = self.active().components[pc_key].grid_pos;

        // Built before taking active_mut(): instantiate needs the document
        // registry (self.documents), so it can't run while a Document borrow
        // from active_mut() is alive - see the borrow rule in the module docs.
        let new_comp = self.instantiate(&new_spec);
        let new_n_in = new_comp.pins.inputs.len();
        let new_n_out = new_comp.pins.outputs.len();

        let doc = self.active_mut();
        doc.history.begin_batch();
        doc.apply(Command::RemoveComponent(old_key));
        let new_key = doc.apply(Command::comp(new_comp)).unwrap_comp();
        // Record the record swap's undo before overwriting: restores the old
        // CompKey + def (the Sim actions above reactivate the old circuit comp
        // and deactivate the new one, but the record itself needs restoring).
        let old_spec = std::mem::replace(
            &mut doc.components[pc_key],
            PlacedComponent::new(new_key, new_spec, grid_pos),
        )
        .spec;
        doc.history.push_gui(GuiUndoAction::SwapComponentSpec {
            key: pc_key,
            comp_key: old_key,
            spec: old_spec,
        });

        let delta = doc.wiring.prune_stale_pins(pc_key, new_n_in, new_n_out);
        doc.edit_wiring(delta);
        doc.sync_component_wire_nodes(pc_key);
        doc.rebuild_circuit();
        doc.history.end_batch();
        doc.selected = Some(Selection::Single(Selected::Component(pc_key)));
    }

    // Draws any open ROM/RAM contents editor windows (see gui::memory_editor)
    // and applies the word edits they collected. Applying stays here because the
    // write paths need &mut Circuit + settle (ROM) that MemoryEditor doesn't own.
    fn show_memory_editors(&mut self, ctx: &egui::Context) {
        let value_locked = self.value_editing_locked();
        let doc = self.active_mut();
        let edits = doc.memory_editor.show(ctx, &doc.components, value_locked);
        for edit in edits {
            match edit.kind {
                MemKind::Rom => doc.write_rom_cell(edit.pc, edit.index, edit.value),
                MemKind::Ram => doc.write_ram_cell(edit.pc, edit.index, edit.value),
            }
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
                self.show_clock_controls(ui);
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

    // The clock transport: a speed setting plus Play / Pause / Step / Stop.
    // Buttons are enable-gated on the current run state (see the state table in
    // ClockRun); entering Play locks editing for the whole session and Stop
    // resets sequential state. All ticks are issued untracked (see tick_once).
    fn show_clock_controls(&mut self, ui: &mut egui::Ui) {
        const MAX_CLOCK_TPS: f32 = 100.0;
        let run = self.active().clock.run;

        // Speed is only adjustable while stopped - locked during a run session.
        ui.add_enabled(
            run == ClockRun::Stopped,
            egui::DragValue::new(&mut self.active_mut().clock.ticks_per_second)
                .speed(0.1)
                .range(1.0..=MAX_CLOCK_TPS)
                .suffix(" tick/s"),
        );

        // Play: start (from Stopped) or resume (from Paused) auto-advancing.
        // Resets the auto-advance clock and abandons any in-progress placement
        // so nothing can edit mid-run.
        if ui
            .add_enabled(run != ClockRun::Playing, egui::Button::new("Play"))
            .clicked()
        {
            let now = ui.ctx().input(|i| i.time);
            let doc = self.active_mut();
            doc.clock.play(now);
            doc.mode = InteractionMode::Idle;
        }

        // Pause: freeze mid-run, preserving sequential state (stays locked).
        if ui
            .add_enabled(run == ClockRun::Playing, egui::Button::new("Pause"))
            .clicked()
        {
            self.active_mut().clock.pause();
        }

        // Step: advance exactly one tick. Available when not playing - from
        // Stopped it's a single manual tick (stays editable); from Paused it
        // nudges the frozen run forward one step.
        if ui
            .add_enabled(run != ClockRun::Playing, egui::Button::new("Step"))
            .clicked()
        {
            self.active_mut().tick_once();
        }

        // Stop: halt, reset all sequential state to power-on, return to editable.
        if ui
            .add_enabled(run != ClockRun::Stopped, egui::Button::new("Stop"))
            .clicked()
        {
            self.active_mut().stop_clock();
        }
    }

    // ── Canvas drawing ────────────────────────────────────────────────────
    fn draw_canvas(&self, painter: &Painter, clip_rect: Rect, camera: Camera, theme: Theme) {
        puffin::profile_function!();
        painter.rect_filled(clip_rect, 0.0, theme.canvas_bg);
        draw_grid(painter, clip_rect, camera, theme);

        let doc = self.active();

        // Draw wire segments. Colour comes from each connected group's net
        // value: any component pin / tunnel on the group resolves (live) to
        // that net's Value; a dangling group (no endpoints) is Floating.
        let node_value = doc.wire_node_values();

        for (seg_key, seg) in doc.wiring.active_segments() {
            let a = doc.wiring.nodes[seg.a];
            let b = doc.wiring.nodes[seg.b];
            let p0 = camera.grid_to_screen(a.pos);
            let p1 = camera.grid_to_screen(b.pos);
            let val = node_value.get(&seg.a).copied().unwrap_or(Value::Floating);
            let mut stroke = value_stroke(theme, val);
            if doc.is_highlighted(Selected::Wire(seg_key)) {
                stroke.color = theme.outline_selected;
                stroke.width += 1.5;
            }
            stroke.width = camera.scale(stroke.width);
            let (p0, p1) = extend_segment(p0, p1, stroke.width / 2.0);
            painter.line_segment([p0, p1], stroke);
        }

        // Junction dots where three or more segments meet, so a real branch
        // reads differently from a mere crossing. All degrees in one pass, not
        // a per-node scan of every segment.
        let degrees = doc.wiring.degrees();
        for (nk, node) in doc.wiring.active_nodes() {
            if degrees.get(&nk).copied().unwrap_or(0) >= 3 {
                let val = node_value.get(&nk).copied().unwrap_or(Value::Floating);
                painter.circle_filled(
                    camera.grid_to_screen(node.pos),
                    camera.scale(PIN_RADIUS),
                    value_stroke(theme, val).color,
                );
            }
        }

        // Draw components

        for (pc_key, pc) in doc.active_components() {
            let is_selected = doc.is_highlighted(Selected::Component(pc_key));
            draw_component(painter, pc, camera, &doc.circuit, is_selected, theme);
        }

        // Draw tunnels

        for (pt_key, pt) in doc.active_tunnels() {
            let is_selected = doc.is_highlighted(Selected::Tunnel(pt_key));
            draw_tunnel(painter, pt, camera, &doc.circuit, is_selected, theme);
        }
    }

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
            InteractionMode::Idle => self.interact_idle(cc, pointer),
            InteractionMode::Placing { spec } => self.interact_placing(cc, pointer, spec),
            InteractionMode::PlacingTunnel { role } => {
                self.interact_placing_tunnel(cc, pointer, role)
            }
            InteractionMode::WireDraw {
                points,
                start_attach,
                cursor,
                dragging,
            } => self.interact_wire_draw(cc, pointer, points, start_attach, cursor, dragging),
            InteractionMode::SelectionDrag {
                items,
                free_nodes,
                drag_origin,
            } => self.interact_component_drag(cc, pointer, items, free_nodes, drag_origin),
            InteractionMode::BulkSelect { start, current } => {
                self.interact_bulk_select(cc, pointer, start, current)
            }
        }
    }

    fn interact_idle(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>) {
        puffin::profile_function!();
        // Computed before taking active_mut(), which borrows self.documents
        // mutably for the rest of this function - see the borrow rule in the
        // document module docs.
        let locked = self.editing_locked();
        let doc = self.active_mut();

        // Hover reticle: hovering over a wire (but not a pin) shows
        // where a branch would tap the wire.
        if let Some(pos) = pointer {
            if pin_at_pos(doc.active_components(), cc.camera, pos, PinKind::Output).is_none()
                && pin_at_pos(doc.active_components(), cc.camera, pos, PinKind::Input).is_none()
                && tunnel_pin_at_pos(doc.active_tunnels(), cc.camera, pos).is_none()
            {
                if let Some((_, gp)) = doc.wiring.segment_at_pos(pos, cc.camera) {
                    draw_reticle(
                        cc.painter,
                        cc.camera.grid_to_screen(gp),
                        cc.camera,
                        cc.theme,
                    );
                }
            }
        }

        // All drag gestures (wire draw, component/bulk move, rubber-band
        // select) mutate the circuit or selection - suppressed during a run
        // session. Plain click-to-select below stays available for inspection.
        // egui's `drag_started`/`dragged` flags are button-agnostic, so exclude
        // a middle-button drag here - that gesture pans the camera
        // (`handle_camera_input`) and must not also start an edit gesture.
        if !locked
            && cc.response.drag_started()
            && !cc.response.dragged_by(egui::PointerButton::Middle)
        {
            let origin = cc.ctx.input(|i| i.pointer.press_origin());
            if let Some(pos) = origin {
                if let Some((attach, gp)) = doc.wire_start_at(pos, cc.camera) {
                    // Drag from a pin / tunnel / existing wire → draw
                    // a wire (quick elbow, committed on release).
                    doc.mode = InteractionMode::WireDraw {
                        points: vec![gp],
                        start_attach: attach,
                        cursor: pos,
                        dragging: true,
                    };
                } else if let Some((items, free_nodes)) = match &doc.selected {
                    Some(Selection::Single(sel)) => {
                        // Selected component/tunnel body drag → move it,
                        // but only when the drag actually began inside its
                        // bounding rect. A lone selected wire has no body to
                        // drag (drag_grid_pos returns None for it).
                        let sel = *sel;
                        doc.drag_grid_pos(sel, cc.camera)
                            .filter(|(rect, _)| rect.contains(pos))
                            .map(|(_, grid_pos)| (vec![(sel, grid_pos)], Vec::new()))
                    }
                    Some(Selection::Bulk(sels)) => {
                        // Bulk body drag → move every selected component/
                        // tunnel together, plus any Free wire node the
                        // selection also covers, as long as the drag began
                        // inside *any one* component/tunnel's bounding rect.
                        let started_inside = sels.iter().any(|sel| {
                            doc.drag_grid_pos(*sel, cc.camera)
                                .is_some_and(|(rect, _)| rect.contains(pos))
                        });
                        started_inside.then(|| {
                            let items: Vec<(Selected, GridPos)> = sels
                                .iter()
                                .filter_map(|sel| {
                                    doc.drag_grid_pos(*sel, cc.camera).map(|(_, gp)| (*sel, gp))
                                })
                                .collect();
                            let free_nodes = doc.free_wire_nodes(sels);
                            (items, free_nodes)
                        })
                    }
                    None => None,
                } {
                    doc.mode = InteractionMode::SelectionDrag {
                        items,
                        free_nodes,
                        drag_origin: pos,
                    };
                } else {
                    // Drag from empty space → rubber-band bulk select.
                    let gp = cc.camera.screen_to_grid(pos);
                    doc.selected = None;
                    doc.mode = InteractionMode::BulkSelect {
                        start: gp,
                        current: gp,
                    };
                }
            }
        }

        if cc.response.clicked() {
            if let Some(pos) = pointer {
                // A click starts a polyline only from a pin/tunnel;
                // clicking a bare wire selects it instead (branching
                // off a wire is a drag gesture, handled above). Suppressed
                // during a run session so only selection remains.
                let pin_start = doc
                    .wire_start_at(pos, cc.camera)
                    .filter(|(a, _)| matches!(a, NodeAttach::Pin(..) | NodeAttach::Tunnel(_)))
                    .filter(|_| !locked);
                if let Some((attach, gp)) = pin_start {
                    doc.mode = InteractionMode::WireDraw {
                        points: vec![gp],
                        start_attach: attach,
                        cursor: pos,
                        dragging: false,
                    };
                } else {
                    // Click a component/tunnel body (components take
                    // priority), then a wire segment, else deselect.
                    let maybe_comp = doc
                        .active_components()
                        .find(|(_k, pc)| component_bounding_rect(pc, cc.camera).contains(pos))
                        .map(|(k, _)| Selected::Component(k));
                    let maybe_tunnel = doc
                        .active_tunnels()
                        .find(|(_k, pt)| tunnel_bounding_rect(pt, cc.camera).contains(pos))
                        .map(|(k, _)| Selected::Tunnel(k));
                    let maybe_wire = doc
                        .wiring
                        .segment_at_pos(pos, cc.camera)
                        .map(|(seg, _)| Selected::Wire(seg));
                    doc.selected = maybe_comp
                        .or(maybe_tunnel)
                        .or(maybe_wire)
                        .map(Selection::Single);
                }
            }
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

    fn interact_placing_tunnel(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>, role: TunnelRole) {
        if let Some(pos) = pointer {
            let gp = cc.camera.screen_to_grid(pos);
            draw_tunnel_ghost(cc.painter, role, gp, cc.camera, cc.theme);
        }
        if cc.response.clicked() {
            if let Some(pos) = pointer {
                let gp = cc.camera.screen_to_grid(pos);
                self.active_mut().place_tunnel(role, gp);
                self.active_mut().mode = InteractionMode::Idle;
            }
        }
    }

    fn interact_wire_draw(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        points: Vec<GridPos>,
        start_attach: NodeAttach,
        cursor: Pos2,
        dragging: bool,
    ) {
        puffin::profile_function!();
        let end = pointer.unwrap_or(cursor);
        let doc = self.active_mut();
        let (drop_attach, drop_gp, terminal) = doc.wire_target_at(end, cc.camera);

        // Preview: committed segments, then the pending elbow from the
        // last committed corner to the (snapped) drop point.
        let preview = Stroke::new(
            cc.camera.scale(WIRE_THICKNESS_THIN),
            cc.theme.wire_drag_preview,
        );
        for w in points.windows(2) {
            cc.painter.line_segment(
                [
                    cc.camera.grid_to_screen(w[0]),
                    cc.camera.grid_to_screen(w[1]),
                ],
                preview,
            );
        }
        let pending = route_elbow(*points.last().unwrap(), drop_gp);
        let mut prev = *points.last().unwrap();
        for p in &pending {
            cc.painter.line_segment(
                [cc.camera.grid_to_screen(prev), cc.camera.grid_to_screen(*p)],
                preview,
            );
            prev = *p;
        }

        if dragging {
            doc.mode = InteractionMode::WireDraw {
                points: points.clone(),
                start_attach,
                cursor: end,
                dragging,
            };
            if cc.response.drag_stopped() {
                let mut route = points.clone();
                route.extend(pending);
                doc.commit_wire_route(route, start_attach, drop_attach);
                doc.mode = InteractionMode::Idle;
            }
        } else {
            // Click-polyline: a click on a terminal (or a double-click)
            // finishes; any other click drops a corner and continues.
            let mut next_points = points.clone();
            let mut finished = false;
            if cc.response.double_clicked() {
                next_points.extend(pending.clone());
                doc.history.begin_batch();
                let delta = doc
                    .wiring
                    .add_route(&next_points, start_attach, NodeAttach::Free);
                doc.edit_wiring(delta);
                finished = true;
            } else if cc.response.clicked() {
                next_points.extend(pending.clone());
                if terminal {
                    doc.history.begin_batch();
                    let delta = doc
                        .wiring
                        .add_route(&next_points, start_attach, drop_attach);
                    doc.edit_wiring(delta);
                    finished = true;
                }
            }
            if finished {
                doc.rebuild_circuit();
                doc.history.end_batch();
                doc.mode = InteractionMode::Idle;
            } else {
                doc.mode = InteractionMode::WireDraw {
                    points: next_points,
                    start_attach,
                    cursor: end,
                    dragging,
                };
            }
        }
    }

    fn interact_component_drag(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        items: Vec<(Selected, GridPos)>,
        free_nodes: Vec<(WireNodeKey, GridPos)>,
        drag_origin: Pos2,
    ) {
        puffin::profile_function!();
        let doc = self.active_mut();
        if let Some(pos) = pointer {
            let step = cc.camera.grid_scale();
            let delta_x = ((pos.x - drag_origin.x) / step).round() as i32;
            let delta_y = ((pos.y - drag_origin.y) / step).round() as i32;
            // Every dragged item moves by the same delta from its own
            // drag-start position, so a bulk drag keeps the selection's
            // relative layout intact.
            for &(key, original_grid_pos) in &items {
                let new_grid_pos =
                    GridPos::new(original_grid_pos.x + delta_x, original_grid_pos.y + delta_y);
                // Moving a component/tunnel drags its wire-anchor nodes
                // along; the rest of each attached segment stretches.
                // Topology is unchanged, so no circuit rebuild is needed.
                match key {
                    Selected::Component(k) => {
                        doc.components[k].grid_pos = new_grid_pos;
                        doc.sync_component_wire_nodes(k);
                    }
                    Selected::Tunnel(k) => {
                        doc.tunnels[k].grid_pos = new_grid_pos;
                        doc.sync_tunnel_wire_nodes(k);
                    }
                    Selected::Wire(_) => {}
                }
            }
            // Free-attached wire-elbow nodes have no owning component/tunnel
            // to carry them along via sync_*_wire_nodes, so they're moved
            // directly by the same delta - otherwise a selected wire run
            // with an interior corner would stay pinned while its ends move.
            for &(key, original_pos) in &free_nodes {
                doc.wiring.nodes[key].pos =
                    GridPos::new(original_pos.x + delta_x, original_pos.y + delta_y);
            }
        }
        if cc.response.drag_stopped() {
            // One undo batch restores every moved item's/node's original
            // position at once, even when only some of them actually moved
            // (e.g. the pointer didn't clear a whole grid cell).
            doc.history.begin_batch();
            for (key, original_grid_pos) in items {
                doc.commit_move(key, original_grid_pos);
            }
            for (key, original_pos) in free_nodes {
                doc.commit_wire_node_move(key, original_pos);
            }
            doc.history.end_batch();
            doc.mode = InteractionMode::Idle;
        }
    }

    fn interact_bulk_select(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        start: GridPos,
        current: GridPos,
    ) {
        puffin::profile_function!();
        // Track the live corner, then paint the rubber-band box.
        let current = pointer
            .map(|p| cc.camera.screen_to_grid(p))
            .unwrap_or(current);
        let rect = selection_screen_rect(start, current, cc.camera);
        let c = cc.theme.outline_selected;
        cc.painter.rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 24),
        );
        cc.painter
            .rect_stroke(rect, 0.0, Stroke::new(1.0, c), StrokeKind::Inside);

        // Finish on release. The `!dragged` guard also recovers from a
        // flick released the same frame it started (drag_stopped never
        // fires in the BulkSelect arm then), so the mode can't stick.
        let doc = self.active_mut();
        if cc.response.drag_stopped() || !cc.response.dragged() {
            let selected_items = doc.items_in_rect(rect, cc.camera);
            // If only one item in bounds, directly select it
            doc.selected = match selected_items.len() {
                0 => None,
                1 => Some(Selection::Single(selected_items[0])),
                _ => Some(Selection::Bulk(selected_items)),
            };
            doc.mode = InteractionMode::Idle;
        } else {
            doc.mode = InteractionMode::BulkSelect { start, current };
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
        self.show_memory_editors(&ctx);

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
                        self.show_properties(ui);
                    });
            });

        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let clip_rect = painter.clip_rect();

        // Update the camera (middle-drag pan, Ctrl+scroll zoom) before drawing so
        // the change applies this same frame.
        self.active_mut().camera.handle_input(&response, &ctx);
        let camera = self.active().camera;

        self.handle_canvas_shortcuts(&ctx);
        self.draw_canvas(&painter, clip_rect, camera, theme);

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
fn selection_screen_rect(start: GridPos, current: GridPos, camera: Camera) -> Rect {
    Rect::from_two_pos(camera.grid_to_screen(start), camera.grid_to_screen(current))
}

// A quick L-elbow (one horizontal then one vertical run) from `from` to `to`,
// returning the intermediate corner (if any) and `to`, but not `from`. Both
// endpoints are on-grid, so the corner is too.
fn route_elbow(from: GridPos, to: GridPos) -> Vec<GridPos> {
    if from == to {
        vec![]
    } else if from.x == to.x || from.y == to.y {
        vec![to] // already axis-aligned: a single straight run
    } else {
        vec![GridPos::new(to.x, from.y), to] // horizontal first, then vertical
    }
}

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
    use crate::gui::history::HistoryEntry;
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

    // Insert a wire (one segment) between a component pin and a tunnel.
    fn connect_pin_tunnel(app: &mut OsmilogApp, c: (PlacedCompKey, PinId), ptk: PlacedTunnelKey) {
        let pc = pin_grid_pos(
            &app.active().components[c.0].shape,
            app.active().components[c.0].grid_pos,
            c.1,
        );
        let pt = tunnel_pin_grid(&app.active().tunnels[ptk]);
        let nc = app.active_mut().wiring.nodes.insert(WireNode {
            pos: pc,
            attach: NodeAttach::Pin(c.0, c.1),
            active: true,
        });
        let nt = app.active_mut().wiring.nodes.insert(WireNode {
            pos: pt,
            attach: NodeAttach::Tunnel(ptk),
            active: true,
        });
        app.active_mut().wiring.segments.insert(WireSegment {
            a: nc,
            b: nt,
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
    fn test_delete_component_drops_wire_nodes_and_refreshes_downstream() {
        // Input -> NOT(g) -> Output, then delete the middle gate: its wire nodes
        // (and their now-orphaned neighbours) must be gone, the circuit component
        // removed, the selection cleared, and the downstream Output refreshed
        // (its input is now Floating).
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

        let g_key = app.active().components[g].key;
        let o_key = app.active().components[o].key;
        assert_eq!(app.active().circuit.read_output(o_key), Value::ZERO); // NOT(1) = 0
        app.active_mut().selected = Some(Selection::Single(Selected::Component(g)));

        app.active_mut().delete_component(g);

        // The placed record is tombstoned (kept for undo), so its key stays
        // valid but the record is inactive rather than gone.
        assert!(app.active().components.contains_key(g));
        assert!(!app.active().components[g].active);
        // Circuit-side removal tombstones (keeps the CompKey for undo), so the
        // component is inactive rather than gone.
        assert!(app
            .active()
            .circuit
            .components
            .get(g_key)
            .is_some_and(|c| !c.active));
        // No wire node references the deleted component; orphan neighbours were
        // cleaned up too, leaving no segments.
        assert!(app
            .active()
            .wiring
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Pin(k, _) if k == g)));
        assert_eq!(app.active().wiring.active_segments().count(), 0);
        assert_eq!(app.active().selected, None);
        // Output's input pin is now Floating.
        assert_eq!(app.active().circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_tunnel_drops_wire_nodes() {
        // A component pin wired to a tunnel: deleting the tunnel removes its wire
        // nodes and clears the selection.
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let t = app
            .active_mut()
            .place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let t_key = app.active().tunnels[t].key;
        connect_pin_tunnel(&mut app, (a, PinId::output(0)), t);
        app.active_mut().rebuild_circuit();
        app.active_mut().selected = Some(Selection::Single(Selected::Tunnel(t)));

        app.active_mut().delete_tunnel(t);

        // Placed record tombstoned (kept for undo): key valid, record inactive.
        assert!(app.active().tunnels.contains_key(t));
        assert!(!app.active().tunnels[t].active);
        // Tombstoned circuit-side (TunnelKey kept for undo): inactive, not gone.
        assert!(app
            .active()
            .circuit
            .tunnels
            .get(t_key)
            .is_some_and(|t| !t.active));
        assert!(app
            .active()
            .wiring
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Tunnel(k) if k == t)));
        assert_eq!(app.active().selected, None);
    }

    #[test]
    fn test_rebuild_circuit_reconciles_tunnel_labels() {
        // Regression for the tunnel-rename bug: if the GUI's PlacedTunnel.label
        // is changed without a matching circuit.rename_tunnel (e.g. the user
        // committed the rename by clicking away rather than pressing Enter),
        // rebuild_circuit must reconcile the circuit's label from the GUI's, so
        // the renamed Feed/Pull pair form one group and the value propagates.
        let mut app = OsmilogApp::empty();
        let inp = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let out = place(&mut app, ComponentSpec::Output);
        let pull = app
            .active_mut()
            .place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let feed = app
            .active_mut()
            .place_tunnel(TunnelRole::Feed, GridPos::new(2, 2));

        connect_pin_tunnel(&mut app, (inp, PinId::output(0)), pull);
        connect_pin_tunnel(&mut app, (out, PinId::input(0)), feed);
        app.active_mut().rebuild_circuit();

        let out_key = app.active().components[out].key;
        assert_eq!(app.active().circuit.read_output(out_key), Value::Floating);

        // GUI label changed only; circuit.rename_tunnel deliberately NOT called.
        let shared = app.active().tunnels[pull].label.clone();
        app.active_mut().tunnels[feed].label = shared;
        app.active_mut().rebuild_circuit();

        assert_eq!(app.active().circuit.read_output(out_key), Value::ONE);
    }

    #[test]
    fn test_bulk_select_box_contains_and_delete() {
        // Two components near the origin and one far away. A box over the origin
        // cluster selects exactly those two; a bulk delete removes them and
        // leaves the far one (and clears the selection).
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let b = app.place_component(ComponentSpec::Output, GridPos::new(2, 2));
        let far = app.place_component(ComponentSpec::Output, GridPos::new(50, 50));
        connect_pins(&mut app, (a, PinId::output(0)), (b, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        let camera = Camera::default();
        let rect = selection_screen_rect(GridPos::new(-2, -2), GridPos::new(12, 12), camera);
        let items = app.active().items_in_rect(rect, camera);
        assert!(items.contains(&Selected::Component(a)));
        assert!(items.contains(&Selected::Component(b)));
        assert!(!items.contains(&Selected::Component(far)));

        app.active_mut().selected = Some(Selection::Bulk(items));
        app.active_mut().delete_bulk();

        // Deleted records are tombstoned (inactive), the untouched one stays active.
        assert!(!app.active().components[a].active);
        assert!(!app.active().components[b].active);
        assert!(app.active().components[far].active);
        assert_eq!(app.active().selected, None);
        // The wire between a and b went with them.
        assert_eq!(app.active().wiring.active_segments().count(), 0);
    }

    #[test]
    fn test_delete_wire_segment_splits_net() {
        // Input -> NOT -> Output as two wires; delete the input->gate wire and
        // the gate's input goes Floating (net split), so the output does too.
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
        let o_key = app.active().components[o].key;
        assert_eq!(app.active().circuit.read_output(o_key), Value::ZERO);

        // Delete the a->g segment (the one touching a's output pin node).
        let seg = app
            .active()
            .wiring
            .segments
            .iter()
            .find(|(_, s)| {
                matches!(app.active().wiring.nodes[s.a].attach, NodeAttach::Pin(k, _) if k == a)
                    || matches!(app.active().wiring.nodes[s.b].attach, NodeAttach::Pin(k, _) if k == a)
            })
            .map(|(k, _)| k)
            .unwrap();
        app.active_mut().delete_wire(seg);

        // g's input is now Floating -> NOT(Floating) = Floating at the output.
        assert_eq!(app.active().circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_wire_records_only_the_wiring_delta() {
        // rebuild_circuit is history-free: its ClearNets/Link net reconstruction
        // is *derived* state that records nothing. So deleting a wire produces
        // exactly one entry - the Gui WiringDelta - with no Sim entries from the
        // relink (which used to pad the batch with RelinkAll + per-link undos).
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
        connect_pins(&mut app, (a, PinId::output(0)), (g, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        let seg = app
            .active()
            .wiring
            .active_segments()
            .next()
            .map(|(k, _)| k)
            .unwrap();
        let stack_before = app.active().history.len();
        app.active_mut().delete_wire(seg);

        assert_eq!(app.active().history.len(), stack_before + 1);
        assert!(matches!(
            app.active().history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }

    #[test]
    fn test_commit_move_pushes_undo_only_when_position_changed() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let original = app.active().components[a].grid_pos;
        let stack_before = app.active().history.len();

        // No movement: nothing pushed.
        app.active_mut()
            .commit_move(Selected::Component(a), original);
        assert_eq!(app.active().history.len(), stack_before);

        // Moved: pushes one MoveComponent entry with the correct old_pos.
        app.active_mut().components[a].grid_pos = GridPos::new(original.x + 3, original.y + 1);
        app.active_mut()
            .commit_move(Selected::Component(a), original);
        assert_eq!(app.active().history.len(), stack_before + 1);
        match app.active().history.last() {
            Some(HistoryEntry::Gui(GuiUndoAction::MoveComponent { key, old_pos })) => {
                assert_eq!(*key, a);
                assert_eq!(*old_pos, original);
            }
            other => panic!("expected Gui(MoveComponent), got {other:?}"),
        }
    }

    #[test]
    fn test_bulk_move_commits_as_one_undo_batch() {
        let mut app = OsmilogApp::empty();
        let a = app.place_component(ComponentSpec::Output, GridPos::new(0, 0));
        let b = app.place_component(ComponentSpec::Output, GridPos::new(10, 0));
        let orig_a = app.active().components[a].grid_pos;
        let orig_b = app.active().components[b].grid_pos;
        let stack_before = app.active().history.len();

        // Mirrors what interact_component_drag's drag_stopped branch does for
        // a Selection::Bulk: every dragged item already moved (one frame at
        // a time, by the same pointer delta - simulated here directly since
        // driving the gesture needs a live egui::Response), then the whole
        // set is committed inside one begin_batch/end_batch.
        app.active_mut().components[a].grid_pos = GridPos::new(orig_a.x + 3, orig_a.y + 2);
        app.active_mut().components[b].grid_pos = GridPos::new(orig_b.x + 3, orig_b.y + 2);

        app.active_mut().history.begin_batch();
        app.active_mut().commit_move(Selected::Component(a), orig_a);
        app.active_mut().commit_move(Selected::Component(b), orig_b);
        app.active_mut().history.end_batch();

        // One batch entry for the whole gesture, not one per item.
        assert_eq!(app.active().history.len(), stack_before + 1);
        assert!(matches!(
            app.active().history.last(),
            Some(HistoryEntry::Batch(_))
        ));

        // One undo restores every item's original position at once.
        app.active_mut().undo();
        assert_eq!(app.active().components[a].grid_pos, orig_a);
        assert_eq!(app.active().components[b].grid_pos, orig_b);

        // One redo replays the whole move again.
        app.active_mut().redo();
        assert_eq!(
            app.active().components[a].grid_pos,
            GridPos::new(orig_a.x + 3, orig_a.y + 2)
        );
        assert_eq!(
            app.active().components[b].grid_pos,
            GridPos::new(orig_b.x + 3, orig_b.y + 2)
        );
    }

    #[test]
    fn test_drag_grid_pos_excludes_wire_selection() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        assert!(app
            .active()
            .drag_grid_pos(Selected::Wire(Default::default()), Camera::default())
            .is_none());
        assert!(app
            .active()
            .drag_grid_pos(Selected::Component(a), Camera::default())
            .is_some());
    }

    // Builds a two-segment route (pin -> Free elbow -> pin) between two
    // freshly placed components, returning the components, the elbow's
    // WireNodeKey, and both segment keys.
    fn place_route_with_elbow(
        app: &mut OsmilogApp,
    ) -> (PlacedCompKey, PlacedCompKey, WireNodeKey, Vec<WireSegKey>) {
        let a = app.place_component(
            ComponentSpec::Input(Input { bits: 1, width: 1 }),
            GridPos::new(0, 0),
        );
        let b = app.place_component(ComponentSpec::Output, GridPos::new(10, 0));
        let pa = pin_grid_pos(
            &app.active().components[a].shape,
            app.active().components[a].grid_pos,
            PinId::output(0),
        );
        let pb = pin_grid_pos(
            &app.active().components[b].shape,
            app.active().components[b].grid_pos,
            PinId::input(0),
        );
        let elbow = GridPos::new(pa.x + 2, pb.y + 4);
        let delta = app.active_mut().wiring.add_route(
            &[pa, elbow, pb],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(b, PinId::input(0)),
        );
        app.active_mut().edit_wiring(delta);
        app.active_mut().rebuild_circuit();

        let elbow_key = app
            .active()
            .wiring
            .active_nodes()
            .find(|(_, n)| matches!(n.attach, NodeAttach::Free))
            .map(|(k, _)| k)
            .unwrap();
        let seg_keys: Vec<WireSegKey> = app
            .active()
            .wiring
            .active_segments()
            .map(|(k, _)| k)
            .collect();
        (a, b, elbow_key, seg_keys)
    }

    #[test]
    fn test_free_wire_nodes_dedupes_shared_elbow_and_excludes_pin_nodes() {
        let mut app = OsmilogApp::empty();
        let (_, _, elbow, segs) = place_route_with_elbow(&mut app);
        assert_eq!(segs.len(), 2, "pin -> elbow -> pin is two segments");

        let sels: Vec<Selected> = segs.iter().map(|&s| Selected::Wire(s)).collect();
        let free_nodes = app.active().free_wire_nodes(&sels);

        // Both segments share the elbow node; it must appear exactly once,
        // and the two Pin-attached endpoints must not appear at all.
        assert_eq!(free_nodes.len(), 1);
        assert_eq!(free_nodes[0].0, elbow);
        assert_eq!(free_nodes[0].1, app.active().wiring.nodes[elbow].pos);
    }

    #[test]
    fn test_bulk_drag_batch_restores_free_wire_node_alongside_components() {
        let mut app = OsmilogApp::empty();
        let (a, b, elbow, _segs) = place_route_with_elbow(&mut app);
        let orig_a = app.active().components[a].grid_pos;
        let orig_b = app.active().components[b].grid_pos;
        let orig_elbow = app.active().wiring.nodes[elbow].pos;

        // What interact_component_drag does for one drag frame of a bulk
        // selection covering both components and the whole wire run: move
        // every component (syncing its pin-attached nodes) and every Free
        // elbow node by the same delta.
        let new_a = GridPos::new(orig_a.x + 3, orig_a.y + 2);
        let new_b = GridPos::new(orig_b.x + 3, orig_b.y + 2);
        let new_elbow = GridPos::new(orig_elbow.x + 3, orig_elbow.y + 2);
        app.active_mut().components[a].grid_pos = new_a;
        app.active_mut().sync_component_wire_nodes(a);
        app.active_mut().components[b].grid_pos = new_b;
        app.active_mut().sync_component_wire_nodes(b);
        app.active_mut().wiring.nodes[elbow].pos = new_elbow;

        // Wire geometry moved as a whole - the elbow isn't left behind.
        assert_eq!(app.active().wiring.nodes[elbow].pos, new_elbow);

        // drag_stopped: commit every moved item and node as one undo batch.
        let stack_before = app.active().history.len();
        app.active_mut().history.begin_batch();
        app.active_mut().commit_move(Selected::Component(a), orig_a);
        app.active_mut().commit_move(Selected::Component(b), orig_b);
        app.active_mut().commit_wire_node_move(elbow, orig_elbow);
        app.active_mut().history.end_batch();
        assert_eq!(app.active().history.len(), stack_before + 1);
        assert!(matches!(
            app.active().history.last(),
            Some(HistoryEntry::Batch(_))
        ));

        // One undo restores the components AND the elbow together.
        app.active_mut().undo();
        assert_eq!(app.active().components[a].grid_pos, orig_a);
        assert_eq!(app.active().components[b].grid_pos, orig_b);
        assert_eq!(app.active().wiring.nodes[elbow].pos, orig_elbow);

        // One redo replays the whole move again.
        app.active_mut().redo();
        assert_eq!(app.active().components[a].grid_pos, new_a);
        assert_eq!(app.active().components[b].grid_pos, new_b);
        assert_eq!(app.active().wiring.nodes[elbow].pos, new_elbow);
    }

    // ── undo / redo ────────────────────────────────────────────────────────

    fn and2() -> ComponentSpec {
        ComponentSpec::Gate(Gate {
            op: GateOp::And,
            n_inputs: 2,
            width: 1,
        })
    }

    #[test]
    fn undo_redo_place_component_toggles_both_records() {
        let mut app = OsmilogApp::empty();
        let g = place(&mut app, and2());
        let comp_key = app.active().components[g].key;
        assert!(app.active().history.can_undo());
        assert!(!app.active().history.can_redo());
        assert!(app.active().components[g].active);
        assert!(app.active().circuit.components[comp_key].active);

        app.active_mut().undo();
        assert!(
            !app.active().components[g].active,
            "record tombstoned by undo"
        );
        assert!(
            !app.active().circuit.components[comp_key].active,
            "circuit component deactivated by undo"
        );
        assert!(app.active().history.can_redo());
        assert!(!app.active().history.can_undo());

        app.active_mut().redo();
        assert!(app.active().components[g].active);
        assert!(app.active().circuit.components[comp_key].active);
        assert!(app.active().history.can_undo());
        assert!(!app.active().history.can_redo());
    }

    #[test]
    fn undo_redo_wire_draw_round_trips_connectivity() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let o = place(&mut app, ComponentSpec::Output);
        app.active_mut().commit_wire_route(
            vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(o, PinId::input(0)),
        );
        let o_key = app.active().components[o].key;
        assert_eq!(app.active().circuit.read_output(o_key), Value::ONE);
        assert_eq!(app.active().wiring.groups().len(), 1);

        app.active_mut().undo();
        assert!(
            app.active()
                .wiring
                .groups()
                .iter()
                .all(|grp| grp.pins.len() < 2),
            "wire removed: no group ties both pins together"
        );
        assert_eq!(app.active().circuit.read_output(o_key), Value::Floating);

        app.active_mut().redo();
        assert_eq!(app.active().wiring.groups().len(), 1);
        assert_eq!(app.active().circuit.read_output(o_key), Value::ONE);
    }

    #[test]
    fn undo_redo_delete_component_restores_wire_and_value() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let o = place(&mut app, ComponentSpec::Output);
        app.active_mut().commit_wire_route(
            vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(o, PinId::input(0)),
        );
        let o_key = app.active().components[o].key;
        assert_eq!(app.active().circuit.read_output(o_key), Value::ONE);

        app.active_mut().delete_component(a);
        assert!(!app.active().components[a].active);
        assert_eq!(app.active().circuit.read_output(o_key), Value::Floating);

        app.active_mut().undo();
        assert!(app.active().components[a].active);
        let a_key = app.active().components[a].key;
        assert!(app.active().circuit.components[a_key].active);
        assert_eq!(
            app.active().circuit.read_output(o_key),
            Value::ONE,
            "wire nodes and driving input restored"
        );

        app.active_mut().redo();
        assert!(!app.active().components[a].active);
        assert_eq!(app.active().circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn undo_redo_reconfigure_restores_def_and_key() {
        let mut app = OsmilogApp::empty();
        let g = place(&mut app, and2());
        let old_key = app.active().components[g].key;
        app.reconfigure_component(
            g,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        assert!(matches!(
            app.active().components[g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                ..
            })
        ));

        app.active_mut().undo();
        assert!(matches!(
            app.active().components[g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::And,
                n_inputs: 2,
                ..
            })
        ));
        assert_eq!(
            app.active().components[g].key,
            old_key,
            "old CompKey restored"
        );

        app.active_mut().redo();
        assert!(matches!(
            app.active().components[g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                ..
            })
        ));
    }

    #[test]
    fn undo_redo_move_restores_grid_pos() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, and2());
        let original = app.active().components[a].grid_pos;
        let moved = GridPos::new(original.x + 4, original.y + 2);
        app.active_mut().components[a].grid_pos = moved;
        app.active_mut()
            .commit_move(Selected::Component(a), original);

        app.active_mut().undo();
        assert_eq!(app.active().components[a].grid_pos, original);

        app.active_mut().redo();
        assert_eq!(app.active().components[a].grid_pos, moved);
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let mut app = OsmilogApp::empty();
        place(&mut app, and2());
        app.active_mut().undo();
        assert!(app.active().history.can_redo());
        // A fresh edit invalidates the redo branch.
        place(&mut app, ComponentSpec::Output);
        assert!(!app.active().history.can_redo());
    }

    #[test]
    fn undo_redo_tunnel_rename_restores_label() {
        let mut app = OsmilogApp::empty();
        let t = app
            .active_mut()
            .place_tunnel(TunnelRole::Feed, GridPos::new(0, 0));
        let tunnel_key = app.active().tunnels[t].key;
        let original = app.active().tunnels[t].label.clone();

        // Simulate a rename commit: record label change live, then the batched
        // Sim rename + record-label undo (mirrors show_tunnel_properties).
        app.active_mut().tunnels[t].label = "RENAMED".to_string();
        app.active_mut().history.begin_batch();
        app.active_mut().apply(Command::RenameTunnel {
            tunnel: tunnel_key,
            new_label: "RENAMED".to_string(),
        });
        app.active_mut()
            .history
            .push_gui(GuiUndoAction::SetTunnelLabel {
                key: t,
                label: original.clone(),
            });
        app.active_mut().history.end_batch();

        app.active_mut().undo();
        assert_eq!(app.active().tunnels[t].label, original);
        assert_eq!(app.active().circuit.tunnels[tunnel_key].label, original);

        app.active_mut().redo();
        assert_eq!(app.active().tunnels[t].label, "RENAMED");
        assert_eq!(app.active().circuit.tunnels[tunnel_key].label, "RENAMED");
    }

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

    #[test]
    fn test_editing_locked_tracks_run_state() {
        let mut app = OsmilogApp::empty();
        // Stopped (initial) is editable.
        assert!(!app.editing_locked());

        // Both Playing and Paused lock the whole run session.
        app.active_mut().clock.run = ClockRun::Playing;
        assert!(app.editing_locked());
        app.active_mut().clock.run = ClockRun::Paused;
        assert!(app.editing_locked());

        // Stop returns to editable.
        app.active_mut().stop_clock();
        assert_eq!(app.active().clock.run, ClockRun::Stopped);
        assert!(!app.editing_locked());
    }

    #[test]
    fn test_value_editing_locked_only_while_playing() {
        let mut app = OsmilogApp::empty();
        // Stopped: fully editable, so value edits are allowed.
        assert!(!app.value_editing_locked());

        // Paused carves value edits (Input bits, ROM/RAM contents) out of the
        // structural lock: still not value-locked, even though editing_locked().
        app.active_mut().clock.run = ClockRun::Paused;
        assert!(app.editing_locked());
        assert!(!app.value_editing_locked());

        // Playing blocks everything, values included.
        app.active_mut().clock.run = ClockRun::Playing;
        assert!(app.value_editing_locked());

        app.active_mut().stop_clock();
        assert!(!app.value_editing_locked());
    }

    #[test]
    fn test_stop_clock_resets_register_through_gui() {
        let mut app = OsmilogApp::empty();
        let data = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let we = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let reg = place(&mut app, ComponentSpec::Reg(RegConf { data_width: 1 }));
        let out = place(&mut app, ComponentSpec::Output);

        connect_pins(&mut app, (data, PinId::output(0)), (reg, PinId::input(0)));
        connect_pins(&mut app, (we, PinId::output(0)), (reg, PinId::input(1)));
        connect_pins(&mut app, (reg, PinId::output(0)), (out, PinId::input(0)));
        app.active_mut().rebuild_circuit();

        let out_key = app.active().components[out].key;
        // Register powers on at 0.
        assert_eq!(app.active().circuit.read_output(out_key), Value::ZERO);

        // A tick with write-enable high latches the data (1).
        app.active_mut().tick_once();
        assert_eq!(app.active().circuit.read_output(out_key), Value::ONE);

        // Stop resets the register to 0 and returns to the editable state.
        app.active_mut().clock.run = ClockRun::Playing;
        app.active_mut().stop_clock();
        assert_eq!(app.active().clock.run, ClockRun::Stopped);
        assert_eq!(app.active().circuit.read_output(out_key), Value::ZERO);
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

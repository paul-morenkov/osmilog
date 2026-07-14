//! Circuit documents: the multiple-circuits-in-memory model.
//!
//! Every circuit document's per-circuit state (`circuit`/`history`/
//! `components`/…) lives in its own `Document`, stored directly in
//! `OsmilogApp::documents: SlotMap<DocId, CircuitDoc>` - active included, so
//! there is a single source of truth with no "parked vs. live" distinction.
//! Exactly one document is *active* (`OsmilogApp::active_id`); switching is
//! just reassigning that id (`OsmilogApp::switch_circuit`) - no `std::mem`
//! moves, no serialization, so no `ComponentSpec`/ROM deep-copy on a switch.
//! `OsmilogApp::active()`/`active_mut()` are the accessors onto the active
//! document's state. Most of the actual per-document behavior (simulation,
//! undo/redo, wiring queries, placement/deletion) lives as methods directly
//! on `Document` below - `OsmilogApp` only keeps the cross-document
//! operations (subcircuit instantiation, save/load, UI) that need the whole
//! `documents` registry in scope.

use std::collections::HashMap;

use egui::{Pos2, Rect};
use slotmap::SlotMap;

use crate::gui::app::{
    component_bounding_rect, pin_at_pos, pin_grid_pos, tunnel_bounding_rect, tunnel_pin_at_pos,
    tunnel_pin_grid, InteractionMode, PinKind, PlacedCompKey, PlacedTunnel, PlacedTunnelKey,
    Selected, Selection,
};
use crate::gui::clock::Clock;
use crate::gui::geometry::{Camera, GridPos};
use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::history::{History, HistoryEntry};
use crate::gui::memory_editor::MemoryEditor;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::wiring::{NodeAttach, WireNodeKey, WireSegKey, Wiring};
use crate::sim::circuit::{Circuit, TunnelKey, TunnelRole};
use crate::sim::command::{Command, CommandOutput};
use crate::sim::component::{CompKey, Component, ComponentSpec, PinId};
use crate::sim::value::Value;

/// Stable identity of a circuit document, independent of display order, and the
/// handle `ComponentSpec::Subcircuit` references. Defined in `sim::component`
/// (so the spec can embed it without a gui dependency) and re-exported here,
/// where the document registry lives.
pub use crate::sim::component::DocId;

/// The per-circuit ("document") state. Every document - active or not - holds
/// exactly one of these directly in its `CircuitDoc::state`; there's no
/// separate "live" copy the active document promotes into. `OsmilogApp::
/// active()`/`active_mut()` reach the active document's fields by indexing
/// `documents[active_id]`.
pub struct Document {
    pub(crate) circuit: Circuit,
    pub(crate) history: History,
    pub(crate) components: SlotMap<PlacedCompKey, PlacedComponent>,
    pub(crate) tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel>,
    pub(crate) wiring: Wiring,
    pub(crate) mode: InteractionMode,
    pub(crate) camera: Camera,
    pub(crate) selected: Option<Selection>,
    pub(crate) clock: Clock,
    pub(crate) memory_editor: MemoryEditor,
    // Per-document settle() error surface (an oscillation, a tunnel conflict,
    // ...). Distinct from OsmilogApp::io_error, which is reserved for File >
    // Save/Load I/O failures - the menu bar shows whichever is set (I/O errors
    // take priority; see the "Menu bar" section of OsmilogApp::ui).
    pub(crate) settle_error: Option<String>,
}

impl Document {
    /// A fresh blank document - the same per-circuit initial values `empty()` uses.
    pub(crate) fn blank() -> Self {
        Self {
            circuit: Circuit::new(),
            history: History::default(),
            components: SlotMap::default(),
            tunnels: SlotMap::default(),
            wiring: Wiring::new(),
            mode: InteractionMode::Idle,
            camera: Camera::default(),
            selected: None,
            clock: Clock::default(),
            memory_editor: MemoryEditor::default(),
            settle_error: None,
        }
    }
}

/// One circuit document: its display name plus its state.
pub struct CircuitDoc {
    pub(crate) name: String,
    pub(crate) state: Document,
}

impl CircuitDoc {
    pub(crate) fn blank(name: String) -> Self {
        Self {
            name,
            state: Document::blank(),
        }
    }
}

/// Default name suggested for a new circuit, e.g. "Circuit 2" for the second
/// document. Only a suggestion (prefilled into the dialog / used when the user
/// clears the field) - names aren't required to be unique; identity is the `DocId`.
pub(crate) fn default_new_circuit_name(documents: &SlotMap<DocId, CircuitDoc>) -> String {
    format!("Circuit {}", documents.len() + 1)
}

impl Document {
    pub(crate) fn record_settle_result<T>(
        &mut self,
        result: Result<T, crate::sim::circuit::SettleError>,
    ) {
        match result {
            Ok(_) => self.settle_error = None,
            Err(e) => self.settle_error = Some(e.to_string()),
        }
    }

    // Advances the clock exactly one tick and records the settle result. See
    // Clock::step - untracked, so it never lands on the undo stack. Used by both
    // the Step button and the auto-advance loop in logic().
    pub(crate) fn tick_once(&mut self) {
        let result = self.clock.step(&mut self.circuit);
        self.record_settle_result(result);
    }

    // Stops the clock (see Clock::stop): resets all sequential state to its
    // power-on value and returns to the editable Stopped state.
    pub(crate) fn stop_clock(&mut self) {
        let result = self.clock.stop(&mut self.circuit);
        self.record_settle_result(result);
    }

    // Auto-advances the clock while Playing, bridging the frame clock
    // (ctx.input(|i| i.time), wasm-safe) and repaint request into Clock::advance
    // and recording the last-fired tick's settle result. All cadence logic lives
    // on Clock.
    pub(crate) fn advance_clock(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let result = self.clock.advance(&mut self.circuit, now, |wait| {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        });
        if let Some(result) = result {
            self.record_settle_result(result);
        }
    }

    // Applies a Command and records its UndoAction into history in one place;
    // callers use it exactly like circuit.apply() (same CommandOutput/unwrap_*
    // chaining) without touching the undo data themselves.
    pub(crate) fn apply(&mut self, command: Command) -> CommandOutput {
        let (output, undo) = self.circuit.apply(command);
        self.history.push_sim(undo);
        output
    }

    // ── Undo / redo ───────────────────────────────────────────────────────────

    // Applies one history entry (reversing what it recorded) and returns the
    // entry that reverses *this* application, for the opposite stack - undo
    // and redo are the same operation in opposite directions.
    //
    // A Batch applies child-last-first; the collected inverses reproduce the
    // original order, so redo of an undone batch replays it forward.
    fn apply_entry(&mut self, entry: HistoryEntry) -> HistoryEntry {
        match entry {
            HistoryEntry::Sim(action) => HistoryEntry::Sim(self.circuit.apply_undo(action)),
            HistoryEntry::Gui(action) => HistoryEntry::Gui(self.apply_gui_undo(action)),
            HistoryEntry::Batch(entries) => {
                let inverses = entries
                    .into_iter()
                    .rev()
                    .map(|e| self.apply_entry(e))
                    .collect();
                HistoryEntry::Batch(inverses)
            }
        }
    }

    // Reverses the most recent edit, moving its inverse onto the redo stack.
    pub(crate) fn undo(&mut self) {
        if let Some(entry) = self.history.pop_undo() {
            let inverse = self.apply_entry(entry);
            self.history.push_redo(inverse);
            self.refresh_after_history();
        }
    }

    // Re-applies the most recently undone edit, moving its inverse back onto the
    // undo stack.
    pub(crate) fn redo(&mut self) {
        if let Some(entry) = self.history.pop_redo() {
            let inverse = self.apply_entry(entry);
            self.history.push_undo(inverse);
            self.refresh_after_history();
        }
    }

    // Restores derived state after an undo/redo: re-sync wire-node geometry
    // (needed for a move-undo, whose MoveComponent carries no wiring delta),
    // clear any selection that may now point at a tombstoned record, then
    // rebuild the circuit's nets + settle.
    fn refresh_after_history(&mut self) {
        let comp_keys: Vec<PlacedCompKey> = self.active_components().map(|(k, _)| k).collect();
        for k in comp_keys {
            self.sync_component_wire_nodes(k);
        }
        let tunnel_keys: Vec<PlacedTunnelKey> = self.active_tunnels().map(|(k, _)| k).collect();
        for k in tunnel_keys {
            self.sync_tunnel_wire_nodes(k);
        }
        self.selected = None;
        self.rebuild_circuit();
    }

    // (Split from the old place_component: the Component is built by
    // OsmilogApp::instantiate, which needs the document registry for
    // subcircuits and so can't live here - see OsmilogApp::place_component.
    // This only handles the per-document record/undo bookkeeping.)
    pub(crate) fn place_component(
        &mut self,
        comp: Component,
        spec: ComponentSpec,
        grid_pos: GridPos,
    ) -> PlacedCompKey {
        self.history.begin_batch();
        let key = self.apply(Command::comp(comp)).unwrap_comp();
        let pc = PlacedComponent::new(key, spec, grid_pos);
        let pc_key = self.components.insert(pc);
        // Record the placement's undo: tombstone this record. Paired with the
        // Sim DeactivateComponent already recorded by apply() above, so undo
        // both drops the circuit component and hides the visual record.
        self.history.push_gui(GuiUndoAction::SetComponentActive {
            key: pc_key,
            active: false,
        });
        self.history.end_batch();
        pc_key
    }

    pub(crate) fn place_tunnel(&mut self, role: TunnelRole, grid_pos: GridPos) -> PlacedTunnelKey {
        let label = format!("Tunnel{}", self.tunnels.len());
        self.place_tunnel_labeled(label, role, grid_pos)
    }

    // Shared by place_tunnel (auto-generated label) and install_circuit_records
    // (label restored from a saved file - tunnels connect to each other by
    // matching label, so a loaded tunnel must keep its exact saved label).
    pub(crate) fn place_tunnel_labeled(
        &mut self,
        label: String,
        role: TunnelRole,
        grid_pos: GridPos,
    ) -> PlacedTunnelKey {
        self.history.begin_batch();
        let key = self
            .apply(Command::AddTunnel {
                label: label.clone(),
                role,
            })
            .unwrap_tunnel();
        let pt = PlacedTunnel {
            key,
            label,
            role,
            grid_pos,
            active: true,
        };
        let pt_key = self.tunnels.insert(pt);
        // Record the placement's undo: tombstone this record (paired with the
        // Sim DeactivateTunnel from apply() above).
        self.history.push_gui(GuiUndoAction::SetTunnelActive {
            key: pt_key,
            active: false,
        });
        self.history.end_batch();
        pt_key
    }

    // Live (non-tombstoned) placed components/tunnels, mirroring
    // Wiring::active_nodes/active_segments. Raw indexing on a known-live key
    // is still fine - a tombstone is simply never iterated.
    pub(crate) fn active_components(&self) -> impl Iterator<Item = (PlacedCompKey, &PlacedComponent)> {
        self.components.iter().filter(|(_, pc)| pc.active)
    }

    pub(crate) fn active_tunnels(&self) -> impl Iterator<Item = (PlacedTunnelKey, &PlacedTunnel)> {
        self.tunnels.iter().filter(|(_, pt)| pt.active)
    }

    // Rebuilds every circuit net from the GUI wiring graph: clear_nets() drops
    // all nets, then each connected wire group is replayed as circuit links
    // (fan-out/driver-conflict handling lives in Circuit::link). A group with
    // no component pin is skipped. Called after any wiring edit.
    pub(crate) fn rebuild_circuit(&mut self) {
        puffin::profile_function!();
        // Reconcile each circuit tunnel's label from its GUI record
        let renames: Vec<(TunnelKey, String)> = self
            .tunnels
            .values()
            .filter(|pt| self.circuit.tunnel_label(pt.key) != Some(pt.label.as_str()))
            .map(|pt| (pt.key, pt.label.clone()))
            .collect();
        for (key, label) in renames {
            self.circuit.apply(Command::RenameTunnel {
                tunnel: key,
                new_label: label,
            });
        }

        self.circuit.apply(Command::ClearNets);
        for group in self.wiring.groups() {
            // Map GUI PlacedCompKeys to live circuit CompKeys; a stale key
            // (component already gone) is dropped.
            let pins: Vec<(CompKey, PinId)> = group
                .pins
                .iter()
                .filter_map(|&(pck, pin)| {
                    self.components
                        .get(pck)
                        .filter(|pc| pc.active)
                        .map(|pc| (pc.key, pin))
                })
                .collect();
            let Some(&(anchor_comp, anchor_pin)) = pins.first() else {
                continue; // no component pin: nothing to drive a net
            };
            for &(comp, pin) in &pins[1..] {
                self.circuit.apply(Command::Link {
                    a: anchor_comp,
                    a_pin: anchor_pin,
                    b: comp,
                    b_pin: pin,
                });
            }
            for &ptk in &group.tunnels {
                if let Some(pt) = self.tunnels.get(ptk).filter(|pt| pt.active) {
                    self.circuit.apply(Command::LinkTunnel {
                        tunnel: pt.key,
                        comp: anchor_comp,
                        pin: anchor_pin,
                    });
                }
            }
        }
        let result = self.circuit.settle();
        self.record_settle_result(result);
    }

    // Repositions the component's wire-anchor nodes to its current pin grid
    // positions (after a move or reconfigure). Segments to them stretch.
    pub(crate) fn sync_component_wire_nodes(&mut self, pck: PlacedCompKey) {
        let Some(pc) = self.components.get(pck) else {
            return;
        };
        let shape = &pc.shape;
        let grid_pos = pc.grid_pos;
        self.wiring
            .sync_component_nodes(pck, |pin| pin_grid_pos(shape, grid_pos, pin));
    }

    pub(crate) fn sync_tunnel_wire_nodes(&mut self, ptk: PlacedTunnelKey) {
        let Some(pt) = self.tunnels.get(ptk) else {
            return;
        };
        self.wiring.sync_tunnel_nodes(ptk, tunnel_pin_grid(pt));
    }

    // The resolved circuit Value at each wire node, for colouring segments. All
    // nodes in a connected group share one net, so we resolve the group's value
    // from any pin/tunnel endpoint on it (Floating if it has none).
    pub(crate) fn wire_node_values(&self) -> HashMap<WireNodeKey, Value> {
        puffin::profile_function!();
        let mut out = HashMap::new();
        for group in self.wiring.groups() {
            let mut val = Value::Floating;
            for &(pck, pin) in &group.pins {
                if let Some(pc) = self.components.get(pck).filter(|pc| pc.active) {
                    if let Some(nk) = self.circuit.components[pc.key].net_of(pin) {
                        val = self.circuit.nets[nk].value;
                        break;
                    }
                }
            }
            if val == Value::Floating {
                for &ptk in &group.tunnels {
                    if let Some(pt) = self.tunnels.get(ptk).filter(|pt| pt.active) {
                        if let Some(nk) = self.circuit.tunnels.get(pt.key).and_then(|t| t.net) {
                            val = self.circuit.nets[nk].value;
                            break;
                        }
                    }
                }
            }
            for nk in group.nodes {
                out.insert(nk, val);
            }
        }
        out
    }

    // Resolves what lies under a screen position for wiring: the attachment
    // to bind, the on-grid point to route to, and whether it's a real
    // terminal vs. empty space. Priority: pin (out, then in), tunnel, wire
    // node, wire segment, else the snapped cursor cell.
    pub(crate) fn wire_target_at(&self, pos: Pos2, camera: Camera) -> (NodeAttach, GridPos, bool) {
        puffin::profile_function!();
        if let Some((pck, pin)) = pin_at_pos(self.active_components(), camera, pos, PinKind::Output)
        {
            let gp = pin_grid_pos(
                &self.components[pck].shape,
                self.components[pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some((pck, pin)) = pin_at_pos(self.active_components(), camera, pos, PinKind::Input)
        {
            let gp = pin_grid_pos(
                &self.components[pck].shape,
                self.components[pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some(ptk) = tunnel_pin_at_pos(self.active_tunnels(), camera, pos) {
            return (
                NodeAttach::Tunnel(ptk),
                tunnel_pin_grid(&self.tunnels[ptk]),
                true,
            );
        }
        if let Some(nk) = self.wiring.node_at_pos(pos, camera) {
            return (NodeAttach::Free, self.wiring.nodes[nk].pos, true);
        }
        if let Some((_, gp)) = self.wiring.segment_at_pos(pos, camera) {
            return (NodeAttach::Free, gp, true);
        }
        (NodeAttach::Free, camera.screen_to_grid(pos), false)
    }

    // A wire may only start on a real terminal (pin, tunnel, or existing wire),
    // not in empty space.
    pub(crate) fn wire_start_at(&self, pos: Pos2, camera: Camera) -> Option<(NodeAttach, GridPos)> {
        let (attach, gp, terminal) = self.wire_target_at(pos, camera);
        terminal.then_some((attach, gp))
    }

    // Writes one word into a placed ROM's contents, then settles so a downstream
    // read updates. The placed spec and the live component share one buffer (see
    // Rom::shared), so write_rom mutates what the spec sees too - no separate
    // mirror write. Deliberately not routed through Command/History: ROM contents
    // are mutated in place and are not undoable (like clock ticks).
    pub(crate) fn write_rom_cell(&mut self, pc: PlacedCompKey, index: usize, value: u32) {
        let comp_key = self.components[pc].key;
        self.circuit.write_rom(comp_key, index, value);
        let result = self.circuit.settle();
        self.record_settle_result(result);
    }

    // Writes one word directly into a placed RAM's contents. Unlike
    // write_rom_cell this never needs a settle(): RAM's data_out is a
    // registered output only updated by tick_clock (see RamCell), so a
    // direct memory edit here has nothing downstream to propagate until the
    // next tick. The placed spec and the live component share one buffer
    // (see Ram::shared), so this mutates what the spec sees too. Not routed
    // through Command/History: RAM contents are mutated in place and are not
    // undoable, like a ROM's.
    pub(crate) fn write_ram_cell(&mut self, pc: PlacedCompKey, index: usize, value: u32) {
        let comp_key = self.components[pc].key;
        self.circuit.write_ram(comp_key, index, value);
    }

    // Removes a placed component: drop it from the circuit and its wire nodes
    // from the wiring graph, then rebuild the circuit's nets from what remains.
    pub(crate) fn delete_component(&mut self, key: PlacedCompKey) {
        self.history.begin_batch();
        let comp_key = self.components[key].key;
        self.apply(Command::RemoveComponent(comp_key));
        let delta = self.wiring.remove_component_nodes(key);
        self.edit_wiring(delta);
        // Tombstone rather than remove: keeps the PlacedCompKey valid so undo can
        // reactivate this record (see PlacedComponent::active).
        self.components[key].active = false;
        self.history
            .push_gui(GuiUndoAction::SetComponentActive { key, active: true });
        if self.selected == Some(Selection::Single(Selected::Component(key))) {
            self.selected = None;
        }
        self.rebuild_circuit();
        self.history.end_batch();
    }

    // Removes a placed tunnel: drop it from the circuit and its wire nodes from
    // the wiring graph, then rebuild.
    pub(crate) fn delete_tunnel(&mut self, key: PlacedTunnelKey) {
        self.history.begin_batch();
        let tunnel_key = self.tunnels[key].key;
        self.apply(Command::RemoveTunnel(tunnel_key));
        let delta = self.wiring.remove_tunnel_nodes(key);
        self.edit_wiring(delta);
        // Tombstone rather than remove (see delete_component).
        self.tunnels[key].active = false;
        self.history
            .push_gui(GuiUndoAction::SetTunnelActive { key, active: true });
        if self.selected == Some(Selection::Single(Selected::Tunnel(key))) {
            self.selected = None;
        }
        self.rebuild_circuit();
        self.history.end_batch();
    }

    // Removes a single wire segment; the wiring graph handles orphan cleanup and
    // any net split, then the circuit is rebuilt.
    pub(crate) fn delete_wire(&mut self, seg: WireSegKey) {
        self.history.begin_batch();
        let delta = self.wiring.delete_segment(seg);
        self.edit_wiring(delta);
        if self.selected == Some(Selection::Single(Selected::Wire(seg))) {
            self.selected = None;
        }
        self.rebuild_circuit();
        self.history.end_batch();
    }

    // True if `sel` is either the single selection or part of the bulk
    // selection, i.e. it should be drawn highlighted.
    pub(crate) fn is_highlighted(&self, sel: Selected) -> bool {
        match &self.selected {
            Some(Selection::Single(s)) => *s == sel,
            Some(Selection::Bulk(items)) => items.contains(&sel),
            None => false,
        }
    }

    // A selected item's screen-space bounding rect and current grid_pos, for
    // deciding whether a drag-start point hits it and what "original
    // position" a ComponentDrag should restore on cancel/undo. `None` for a
    // Selected::Wire (no draggable body) or a stale key.
    pub(crate) fn drag_grid_pos(&self, sel: Selected, camera: Camera) -> Option<(Rect, GridPos)> {
        match sel {
            Selected::Component(key) => self
                .components
                .get(key)
                .filter(|pc| pc.active)
                .map(|pc| (component_bounding_rect(pc, camera), pc.grid_pos)),
            Selected::Tunnel(key) => self
                .tunnels
                .get(key)
                .filter(|pt| pt.active)
                .map(|pt| (tunnel_bounding_rect(pt, camera), pt.grid_pos)),
            Selected::Wire(_) => None,
        }
    }

    // The Free-attached WireNodes belonging to any Selected::Wire in `sels`
    // (deduped - a route's interior node can be shared by two selected
    // segments), each paired with its current grid_pos. Pin/Tunnel-attached
    // nodes are excluded: those follow their owning component/tunnel via
    // sync_component_wire_nodes/sync_tunnel_wire_nodes instead, and must
    // stay put if that owner isn't itself part of the drag.
    pub(crate) fn free_wire_nodes(&self, sels: &[Selected]) -> Vec<(WireNodeKey, GridPos)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for sel in sels {
            let Selected::Wire(seg) = sel else { continue };
            let Some(segment) = self.wiring.segments.get(*seg) else {
                continue;
            };
            for node_key in [segment.a, segment.b] {
                if !seen.insert(node_key) {
                    continue;
                }
                if let Some(node) = self.wiring.nodes.get(node_key) {
                    if matches!(node.attach, NodeAttach::Free) {
                        out.push((node_key, node.pos));
                    }
                }
            }
        }
        out
    }

    // Every component, tunnel, and wire segment fully contained in `rect`
    // (screen space): a component/tunnel counts when its bounding rect is
    // inside, a wire when both endpoints are.
    pub(crate) fn items_in_rect(&self, rect: Rect, camera: Camera) -> Vec<Selected> {
        puffin::profile_function!();
        let mut out = Vec::new();
        for (key, pc) in self.active_components() {
            if rect.contains_rect(component_bounding_rect(pc, camera)) {
                out.push(Selected::Component(key));
            }
        }
        for (key, pt) in self.active_tunnels() {
            if rect.contains_rect(tunnel_bounding_rect(pt, camera)) {
                out.push(Selected::Tunnel(key));
            }
        }
        for (key, seg) in self.wiring.active_segments() {
            let a = camera.grid_to_screen(self.wiring.nodes[seg.a].pos);
            let b = camera.grid_to_screen(self.wiring.nodes[seg.b].pos);
            if rect.contains(a) && rect.contains(b) {
                out.push(Selected::Wire(key));
            }
        }
        out
    }

    // Removes everything in the current bulk selection: wire segments first,
    // then components/tunnels (which drop their own wire nodes too). Each
    // removal is existence-checked since deleting a component can take a
    // wire in the same set with it. Rebuilds once at the end.
    pub(crate) fn delete_bulk(&mut self) {
        let Some(Selection::Bulk(items)) = self.selected.take() else {
            return;
        };
        self.history.begin_batch();
        for sel in &items {
            if let Selected::Wire(seg) = *sel {
                // delete_segment self-guards: an already-tombstoned segment
                // (e.g. dropped by a component deletion earlier in this batch)
                // yields an empty delta, so edit_wiring records nothing.
                let delta = self.wiring.delete_segment(seg);
                self.edit_wiring(delta);
            }
        }
        for sel in &items {
            match *sel {
                Selected::Component(key) => {
                    // Guard on active, not just presence: a tombstoned record
                    // (deleted earlier in this same batch) must not be redeleted.
                    if let Some(pc) = self.components.get(key).filter(|pc| pc.active) {
                        let comp_key = pc.key;
                        self.apply(Command::RemoveComponent(comp_key));
                        let delta = self.wiring.remove_component_nodes(key);
                        self.edit_wiring(delta);
                        self.components[key].active = false;
                        self.history
                            .push_gui(GuiUndoAction::SetComponentActive { key, active: true });
                    }
                }
                Selected::Tunnel(key) => {
                    if let Some(pt) = self.tunnels.get(key).filter(|pt| pt.active) {
                        let tunnel_key = pt.key;
                        self.apply(Command::RemoveTunnel(tunnel_key));
                        let delta = self.wiring.remove_tunnel_nodes(key);
                        self.edit_wiring(delta);
                        self.tunnels[key].active = false;
                        self.history
                            .push_gui(GuiUndoAction::SetTunnelActive { key, active: true });
                    }
                }
                Selected::Wire(_) => {}
            }
        }
        self.rebuild_circuit();
        self.history.end_batch();
    }
}

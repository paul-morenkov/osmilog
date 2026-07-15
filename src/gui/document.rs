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

use egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind};
use slotmap::SlotMap;

use crate::gui::app::{
    component_bounding_rect, pin_at_pos, pin_grid_pos, tunnel_bounding_rect, tunnel_pin_at_pos,
    tunnel_pin_grid, InteractionMode, PinKind, PlacedCompKey, PlacedTunnel, PlacedTunnelKey,
    Selected, Selection, PIN_RADIUS, WIRE_THICKNESS_THIN,
};
use crate::gui::canvas_draw::{
    draw_component, draw_grid, draw_reticle, draw_tunnel, draw_tunnel_ghost, extend_segment,
    value_stroke,
};
use crate::gui::clock::{Clock, ClockRun};
use crate::gui::geometry::{Camera, GridPos};
use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::history::{History, HistoryEntry};
use crate::gui::memory_editor::{MemKind, MemoryEditor};
use crate::gui::placed_component::PlacedComponent;
use crate::gui::theme::Theme;
use crate::gui::utils::CanvasCtx;
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
    pub(crate) components: HashMap<PlacedCompKey, PlacedComponent>,
    pub(crate) tunnels: HashMap<PlacedTunnelKey, PlacedTunnel>,
    // Monotonic id allocators for the two record maps; never reused, so undo
    // re-inserts a deleted record under its original key with no aliasing.
    pub(crate) next_placed_comp: u64,
    pub(crate) next_placed_tunnel: u64,
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
            components: HashMap::default(),
            tunnels: HashMap::default(),
            next_placed_comp: 0,
            next_placed_tunnel: 0,
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
    // True while a clock run session is active (Playing or Paused): structural
    // edits (widths, arity, wiring, add/delete) are locked for its whole
    // duration. Only Stop returns to editable. Both lock predicates live here
    // (not just on OsmilogApp) so the properties panel can gate its widgets
    // from a bare `&Document` - see OsmilogApp::editing_locked, which delegates.
    pub(crate) fn editing_locked(&self) -> bool {
        self.clock.run != ClockRun::Stopped
    }

    // True while live *value* edits must be blocked - an Input's bits, a ROM's
    // or RAM's contents. Carved out of the lock while Paused (a paused run is
    // frozen structurally but still pokeable), so this is true only while
    // actively Playing. See OsmilogApp::value_editing_locked, which delegates.
    pub(crate) fn value_editing_locked(&self) -> bool {
        self.clock.run == ClockRun::Playing
    }

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
        let pc_key = PlacedCompKey(self.next_placed_comp);
        self.next_placed_comp += 1;
        self.components.insert(pc_key, pc);
        // Record the placement's undo: remove this record. Its InsertComponent
        // inverse carries it back for redo. Paired with the Sim RemoveComponent
        // already recorded by apply() above, so undo both drops the circuit
        // component and removes the visual record.
        self.history
            .push_gui(GuiUndoAction::RemoveComponent { key: pc_key });
        self.history.end_batch();
        pc_key
    }

    // Swaps a placed component's parameters. PlacedCompKey stays stable, so
    // attached wires survive - we only drop wire nodes for pins the new arity
    // no longer has, re-sync the rest, then rebuild. `new_comp` is built by the
    // caller (`OsmilogApp::reconfigure_component`) via `instantiate`, the one
    // step needing the document registry a `Document` doesn't have.
    pub(crate) fn reconfigure_component(
        &mut self,
        pc_key: PlacedCompKey,
        new_spec: ComponentSpec,
        new_comp: Component,
    ) {
        let old_key = self.components[&pc_key].key;
        let grid_pos = self.components[&pc_key].grid_pos;
        let new_n_in = new_comp.pins.inputs.len();
        let new_n_out = new_comp.pins.outputs.len();

        self.history.begin_batch();
        self.apply(Command::RemoveComponent(old_key));
        let new_key = self.apply(Command::comp(new_comp)).unwrap_comp();
        // Record the record swap's undo before overwriting: restores the old
        // CompKey + def (the Sim actions above re-insert the old circuit comp
        // and remove the new one, but the record itself needs restoring).
        let old_spec = self
            .components
            .insert(pc_key, PlacedComponent::new(new_key, new_spec, grid_pos))
            .expect("reconfigure of a live component")
            .spec;
        self.history.push_gui(GuiUndoAction::SwapComponentSpec {
            key: pc_key,
            comp_key: old_key,
            spec: old_spec,
        });

        let delta = self.wiring.prune_stale_pins(pc_key, new_n_in, new_n_out);
        self.edit_wiring(delta);
        self.sync_component_wire_nodes(pc_key);
        self.rebuild_circuit();
        self.history.end_batch();
        self.selected = Some(Selection::Single(Selected::Component(pc_key)));
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
        };
        let pt_key = PlacedTunnelKey(self.next_placed_tunnel);
        self.next_placed_tunnel += 1;
        self.tunnels.insert(pt_key, pt);
        // Record the placement's undo: remove this record (paired with the Sim
        // RemoveTunnel from apply() above).
        self.history
            .push_gui(GuiUndoAction::RemoveTunnel { key: pt_key });
        self.history.end_batch();
        pt_key
    }

    // Live (non-tombstoned) placed components/tunnels, mirroring
    // Wiring::active_nodes/active_segments. Raw indexing on a known-live key
    // is still fine - a tombstone is simply never iterated.
    pub(crate) fn active_components(
        &self,
    ) -> impl Iterator<Item = (PlacedCompKey, &PlacedComponent)> {
        self.components.iter().map(|(k, pc)| (*k, pc))
    }

    pub(crate) fn active_tunnels(&self) -> impl Iterator<Item = (PlacedTunnelKey, &PlacedTunnel)> {
        self.tunnels.iter().map(|(k, pt)| (*k, pt))
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
                .filter_map(|&(pck, pin)| self.components.get(&pck).map(|pc| (pc.key, pin)))
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
                if let Some(pt) = self.tunnels.get(&ptk) {
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
        let Some(pc) = self.components.get(&pck) else {
            return;
        };
        let shape = &pc.shape;
        let grid_pos = pc.grid_pos;
        self.wiring
            .sync_component_nodes(pck, |pin| pin_grid_pos(shape, grid_pos, pin));
    }

    pub(crate) fn sync_tunnel_wire_nodes(&mut self, ptk: PlacedTunnelKey) {
        let Some(pt) = self.tunnels.get(&ptk) else {
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
                if let Some(pc) = self.components.get(&pck) {
                    if let Some(nk) = self.circuit.components[&pc.key].net_of(pin) {
                        val = self.circuit.nets[nk].value;
                        break;
                    }
                }
            }
            if val == Value::Floating {
                for &ptk in &group.tunnels {
                    if let Some(pt) = self.tunnels.get(&ptk) {
                        if let Some(nk) = self.circuit.tunnels.get(&pt.key).and_then(|t| t.net) {
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

    // Draws the whole canvas: grid, wires (coloured by their group's live
    // value), junction dots, components, and tunnels.
    pub(crate) fn draw(&self, painter: &Painter, clip_rect: Rect, camera: Camera, theme: Theme) {
        puffin::profile_function!();
        painter.rect_filled(clip_rect, 0.0, theme.canvas_bg);
        draw_grid(painter, clip_rect, camera, theme);

        // Draw wire segments. Colour comes from each connected group's net
        // value: any component pin / tunnel on the group resolves (live) to
        // that net's Value; a dangling group (no endpoints) is Floating.
        let node_value = self.wire_node_values();

        for (seg_key, seg) in self.wiring.active_segments() {
            let a = self.wiring.nodes[&seg.a];
            let b = self.wiring.nodes[&seg.b];
            let p0 = camera.grid_to_screen(a.pos);
            let p1 = camera.grid_to_screen(b.pos);
            let val = node_value.get(&seg.a).copied().unwrap_or(Value::Floating);
            let mut stroke = value_stroke(theme, val);
            if self.is_highlighted(Selected::Wire(seg_key)) {
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
        let degrees = self.wiring.degrees();
        for (nk, node) in &self.wiring.nodes {
            if degrees.get(nk).copied().unwrap_or(0) >= 3 {
                let val = node_value.get(nk).copied().unwrap_or(Value::Floating);
                painter.circle_filled(
                    camera.grid_to_screen(node.pos),
                    camera.scale(PIN_RADIUS),
                    value_stroke(theme, val).color,
                );
            }
        }

        // Draw components

        for (pc_key, pc) in self.active_components() {
            let is_selected = self.is_highlighted(Selected::Component(pc_key));
            draw_component(painter, pc, camera, &self.circuit, is_selected, theme);
        }

        // Draw tunnels

        for (pt_key, pt) in self.active_tunnels() {
            let is_selected = self.is_highlighted(Selected::Tunnel(pt_key));
            draw_tunnel(painter, pt, camera, &self.circuit, is_selected, theme);
        }
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
                &self.components[&pck].shape,
                self.components[&pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some((pck, pin)) = pin_at_pos(self.active_components(), camera, pos, PinKind::Input)
        {
            let gp = pin_grid_pos(
                &self.components[&pck].shape,
                self.components[&pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some(ptk) = tunnel_pin_at_pos(self.active_tunnels(), camera, pos) {
            return (
                NodeAttach::Tunnel(ptk),
                tunnel_pin_grid(&self.tunnels[&ptk]),
                true,
            );
        }
        if let Some(nk) = self.wiring.node_at_pos(pos, camera) {
            return (NodeAttach::Free, self.wiring.nodes[&nk].pos, true);
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
        let comp_key = self.components[&pc].key;
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
        let comp_key = self.components[&pc].key;
        self.circuit.write_ram(comp_key, index, value);
    }

    // Draws any open ROM/RAM contents editor windows (see gui::memory_editor)
    // and applies the word edits they collected. Applying stays here because the
    // write paths need &mut Circuit + settle (ROM) that MemoryEditor doesn't own.
    pub(crate) fn show_memory_editors(&mut self, ctx: &egui::Context) {
        let value_locked = self.value_editing_locked();
        let edits = self.memory_editor.show(ctx, &self.components, value_locked);
        for edit in edits {
            match edit.kind {
                MemKind::Rom => self.write_rom_cell(edit.pc, edit.index, edit.value),
                MemKind::Ram => self.write_ram_cell(edit.pc, edit.index, edit.value),
            }
        }
    }

    // Removes a placed component: drop it from the circuit and its wire nodes
    // from the wiring graph, then rebuild the circuit's nets from what remains.
    pub(crate) fn delete_component(&mut self, key: PlacedCompKey) {
        self.history.begin_batch();
        let comp_key = self.components[&key].key;
        self.apply(Command::RemoveComponent(comp_key));
        let delta = self.wiring.remove_component_nodes(key);
        self.edit_wiring(delta);
        // Remove the record outright, moving it into the undo entry; undo
        // re-inserts it under this same PlacedCompKey (see apply_gui_undo).
        if let Some(pc) = self.components.remove(&key) {
            self.history.push_gui(GuiUndoAction::InsertComponent {
                key,
                comp: Box::new(pc),
            });
        }
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
        let tunnel_key = self.tunnels[&key].key;
        self.apply(Command::RemoveTunnel(tunnel_key));
        let delta = self.wiring.remove_tunnel_nodes(key);
        self.edit_wiring(delta);
        // Remove the record outright, moving it into the undo entry (see
        // delete_component).
        if let Some(pt) = self.tunnels.remove(&key) {
            self.history.push_gui(GuiUndoAction::InsertTunnel {
                key,
                tunnel: Box::new(pt),
            });
        }
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
                .get(&key)
                .map(|pc| (component_bounding_rect(pc, camera), pc.grid_pos)),
            Selected::Tunnel(key) => self
                .tunnels
                .get(&key)
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
            let Some(segment) = self.wiring.segments.get(seg) else {
                continue;
            };
            for node_key in [segment.a, segment.b] {
                if !seen.insert(node_key) {
                    continue;
                }
                if let Some(node) = self.wiring.nodes.get(&node_key) {
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
            let a = camera.grid_to_screen(self.wiring.nodes[&seg.a].pos);
            let b = camera.grid_to_screen(self.wiring.nodes[&seg.b].pos);
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
                    // Guard on presence: a record already removed earlier in this
                    // same batch (e.g. by a component deletion) must not be redeleted.
                    if let Some(comp_key) = self.components.get(&key).map(|pc| pc.key) {
                        self.apply(Command::RemoveComponent(comp_key));
                        let delta = self.wiring.remove_component_nodes(key);
                        self.edit_wiring(delta);
                        if let Some(pc) = self.components.remove(&key) {
                            self.history.push_gui(GuiUndoAction::InsertComponent {
                                key,
                                comp: Box::new(pc),
                            });
                        }
                    }
                }
                Selected::Tunnel(key) => {
                    if let Some(tunnel_key) = self.tunnels.get(&key).map(|pt| pt.key) {
                        self.apply(Command::RemoveTunnel(tunnel_key));
                        let delta = self.wiring.remove_tunnel_nodes(key);
                        self.edit_wiring(delta);
                        if let Some(pt) = self.tunnels.remove(&key) {
                            self.history.push_gui(GuiUndoAction::InsertTunnel {
                                key,
                                tunnel: Box::new(pt),
                            });
                        }
                    }
                }
                Selected::Wire(_) => {}
            }
        }
        self.rebuild_circuit();
        self.history.end_batch();
    }
}

// ── Canvas mode dispatch ─────────────────────────────────────────────────
//
// One method per InteractionMode variant (see OsmilogApp::handle_canvas_
// interaction, the dispatcher). Every variant except Placing only needs the
// active document plus the ambient CanvasCtx (egui handles, no app state) -
// Placing needs OsmilogApp::place_component (instantiate, for subcircuits),
// so it stays a thin OsmilogApp method; the dispatcher calls into `self` for
// everything else.
impl Document {
    pub(crate) fn interact_idle(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>) {
        puffin::profile_function!();
        let locked = self.editing_locked();

        // Hover reticle: hovering over a wire (but not a pin) shows
        // where a branch would tap the wire.
        if let Some(pos) = pointer {
            if pin_at_pos(self.active_components(), cc.camera, pos, PinKind::Output).is_none()
                && pin_at_pos(self.active_components(), cc.camera, pos, PinKind::Input).is_none()
                && tunnel_pin_at_pos(self.active_tunnels(), cc.camera, pos).is_none()
            {
                if let Some((_, gp)) = self.wiring.segment_at_pos(pos, cc.camera) {
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
                if let Some((attach, gp)) = self.wire_start_at(pos, cc.camera) {
                    // Drag from a pin / tunnel / existing wire → draw
                    // a wire (quick elbow, committed on release).
                    self.mode = InteractionMode::WireDraw {
                        points: vec![gp],
                        start_attach: attach,
                        cursor: pos,
                        dragging: true,
                    };
                } else if let Some((items, free_nodes)) = match &self.selected {
                    Some(Selection::Single(sel)) => {
                        // Selected component/tunnel body drag → move it,
                        // but only when the drag actually began inside its
                        // bounding rect. A lone selected wire has no body to
                        // drag (drag_grid_pos returns None for it).
                        let sel = *sel;
                        self.drag_grid_pos(sel, cc.camera)
                            .filter(|(rect, _)| rect.contains(pos))
                            .map(|(_, grid_pos)| (vec![(sel, grid_pos)], Vec::new()))
                    }
                    Some(Selection::Bulk(sels)) => {
                        // Bulk body drag → move every selected component/
                        // tunnel together, plus any Free wire node the
                        // selection also covers, as long as the drag began
                        // inside *any one* component/tunnel's bounding rect.
                        let started_inside = sels.iter().any(|sel| {
                            self.drag_grid_pos(*sel, cc.camera)
                                .is_some_and(|(rect, _)| rect.contains(pos))
                        });
                        started_inside.then(|| {
                            let items: Vec<(Selected, GridPos)> = sels
                                .iter()
                                .filter_map(|sel| {
                                    self.drag_grid_pos(*sel, cc.camera)
                                        .map(|(_, gp)| (*sel, gp))
                                })
                                .collect();
                            let free_nodes = self.free_wire_nodes(sels);
                            (items, free_nodes)
                        })
                    }
                    None => None,
                } {
                    self.mode = InteractionMode::SelectionDrag {
                        items,
                        free_nodes,
                        drag_origin: pos,
                    };
                } else {
                    // Drag from empty space → rubber-band bulk select.
                    let gp = cc.camera.screen_to_grid(pos);
                    self.selected = None;
                    self.mode = InteractionMode::BulkSelect {
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
                let pin_start = self
                    .wire_start_at(pos, cc.camera)
                    .filter(|(a, _)| matches!(a, NodeAttach::Pin(..) | NodeAttach::Tunnel(_)))
                    .filter(|_| !locked);
                if let Some((attach, gp)) = pin_start {
                    self.mode = InteractionMode::WireDraw {
                        points: vec![gp],
                        start_attach: attach,
                        cursor: pos,
                        dragging: false,
                    };
                } else {
                    // Click a component/tunnel body (components take
                    // priority), then a wire segment, else deselect.
                    let maybe_comp = self
                        .active_components()
                        .find(|(_k, pc)| component_bounding_rect(pc, cc.camera).contains(pos))
                        .map(|(k, _)| Selected::Component(k));
                    let maybe_tunnel = self
                        .active_tunnels()
                        .find(|(_k, pt)| tunnel_bounding_rect(pt, cc.camera).contains(pos))
                        .map(|(k, _)| Selected::Tunnel(k));
                    let maybe_wire = self
                        .wiring
                        .segment_at_pos(pos, cc.camera)
                        .map(|(seg, _)| Selected::Wire(seg));
                    self.selected = maybe_comp
                        .or(maybe_tunnel)
                        .or(maybe_wire)
                        .map(Selection::Single);
                }
            }
        }
    }

    pub(crate) fn interact_placing_tunnel(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        role: TunnelRole,
    ) {
        if let Some(pos) = pointer {
            let gp = cc.camera.screen_to_grid(pos);
            draw_tunnel_ghost(cc.painter, role, gp, cc.camera, cc.theme);
        }
        if cc.response.clicked() {
            if let Some(pos) = pointer {
                let gp = cc.camera.screen_to_grid(pos);
                self.place_tunnel(role, gp);
                self.mode = InteractionMode::Idle;
            }
        }
    }

    pub(crate) fn interact_wire_draw(
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
        let (drop_attach, drop_gp, terminal) = self.wire_target_at(end, cc.camera);

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
            self.mode = InteractionMode::WireDraw {
                points: points.clone(),
                start_attach,
                cursor: end,
                dragging,
            };
            if cc.response.drag_stopped() {
                let mut route = points.clone();
                route.extend(pending);
                self.commit_wire_route(route, start_attach, drop_attach);
                self.mode = InteractionMode::Idle;
            }
        } else {
            // Click-polyline: a click on a terminal (or a double-click)
            // finishes; any other click drops a corner and continues.
            let mut next_points = points.clone();
            let mut finished = false;
            if cc.response.double_clicked() {
                next_points.extend(pending.clone());
                self.history.begin_batch();
                let delta = self
                    .wiring
                    .add_route(&next_points, start_attach, NodeAttach::Free);
                self.edit_wiring(delta);
                finished = true;
            } else if cc.response.clicked() {
                next_points.extend(pending.clone());
                if terminal {
                    self.history.begin_batch();
                    let delta = self
                        .wiring
                        .add_route(&next_points, start_attach, drop_attach);
                    self.edit_wiring(delta);
                    finished = true;
                }
            }
            if finished {
                self.rebuild_circuit();
                self.history.end_batch();
                self.mode = InteractionMode::Idle;
            } else {
                self.mode = InteractionMode::WireDraw {
                    points: next_points,
                    start_attach,
                    cursor: end,
                    dragging,
                };
            }
        }
    }

    pub(crate) fn interact_component_drag(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        items: Vec<(Selected, GridPos)>,
        free_nodes: Vec<(WireNodeKey, GridPos)>,
        drag_origin: Pos2,
    ) {
        puffin::profile_function!();
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
                        self.components.get_mut(&k).unwrap().grid_pos = new_grid_pos;
                        self.sync_component_wire_nodes(k);
                    }
                    Selected::Tunnel(k) => {
                        self.tunnels.get_mut(&k).unwrap().grid_pos = new_grid_pos;
                        self.sync_tunnel_wire_nodes(k);
                    }
                    Selected::Wire(_) => {}
                }
            }
            // Free-attached wire-elbow nodes have no owning component/tunnel
            // to carry them along via sync_*_wire_nodes, so they're moved
            // directly by the same delta - otherwise a selected wire run
            // with an interior corner would stay pinned while its ends move.
            for &(key, original_pos) in &free_nodes {
                self.wiring.nodes.get_mut(&key).unwrap().pos =
                    GridPos::new(original_pos.x + delta_x, original_pos.y + delta_y);
            }
        }
        if cc.response.drag_stopped() {
            // One undo batch restores every moved item's/node's original
            // position at once, even when only some of them actually moved
            // (e.g. the pointer didn't clear a whole grid cell).
            self.history.begin_batch();
            for (key, original_grid_pos) in items {
                self.commit_move(key, original_grid_pos);
            }
            for (key, original_pos) in free_nodes {
                self.commit_wire_node_move(key, original_pos);
            }
            self.history.end_batch();
            self.mode = InteractionMode::Idle;
        }
    }

    pub(crate) fn interact_bulk_select(
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
        if cc.response.drag_stopped() || !cc.response.dragged() {
            let selected_items = self.items_in_rect(rect, cc.camera);
            // If only one item in bounds, directly select it
            self.selected = match selected_items.len() {
                0 => None,
                1 => Some(Selection::Single(selected_items[0])),
                _ => Some(Selection::Bulk(selected_items)),
            };
            self.mode = InteractionMode::Idle;
        } else {
            self.mode = InteractionMode::BulkSelect { start, current };
        }
    }
}

// The screen-space rectangle spanned by a BulkSelect drag's two grid corners,
// normalized so either drag direction yields the same box.
pub(crate) fn selection_screen_rect(start: GridPos, current: GridPos, camera: Camera) -> Rect {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::wiring::WireNode;
    use crate::sim::component::{Gate, GateOp, Input, RegConf};

    fn place(doc: &mut Document, spec: ComponentSpec) -> PlacedCompKey {
        place_at(doc, spec, GridPos::new(0, 0))
    }

    fn place_at(doc: &mut Document, spec: ComponentSpec, grid_pos: GridPos) -> PlacedCompKey {
        let comp = spec.to_component();
        doc.place_component(comp, spec, grid_pos)
    }

    // Insert a wire (one segment) between two component pins, positioned at each
    // pin's grid cell, and return the two node keys.
    fn connect_pins(doc: &mut Document, a: (PlacedCompKey, PinId), b: (PlacedCompKey, PinId)) {
        let pa = pin_grid_pos(
            &doc.components[&a.0].shape,
            doc.components[&a.0].grid_pos,
            a.1,
        );
        let pb = pin_grid_pos(
            &doc.components[&b.0].shape,
            doc.components[&b.0].grid_pos,
            b.1,
        );
        let na = doc.wiring.insert_node_untracked(WireNode {
            pos: pa,
            attach: NodeAttach::Pin(a.0, a.1),
        });
        let nb = doc.wiring.insert_node_untracked(WireNode {
            pos: pb,
            attach: NodeAttach::Pin(b.0, b.1),
        });
        doc.wiring.insert_segment_untracked(na, nb);
    }

    // Insert a wire (one segment) between a component pin and a tunnel.
    fn connect_pin_tunnel(doc: &mut Document, c: (PlacedCompKey, PinId), ptk: PlacedTunnelKey) {
        let pc = pin_grid_pos(
            &doc.components[&c.0].shape,
            doc.components[&c.0].grid_pos,
            c.1,
        );
        let pt = tunnel_pin_grid(&doc.tunnels[&ptk]);
        let nc = doc.wiring.insert_node_untracked(WireNode {
            pos: pc,
            attach: NodeAttach::Pin(c.0, c.1),
        });
        let nt = doc.wiring.insert_node_untracked(WireNode {
            pos: pt,
            attach: NodeAttach::Tunnel(ptk),
        });
        doc.wiring.insert_segment_untracked(nc, nt);
    }

    #[test]
    fn test_delete_component_drops_wire_nodes_and_refreshes_downstream() {
        // Input -> NOT(g) -> Output, then delete the middle gate: its wire nodes
        // (and their now-orphaned neighbours) must be gone, the circuit component
        // removed, the selection cleared, and the downstream Output refreshed
        // (its input is now Floating).
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut doc,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        let o = place(&mut doc, ComponentSpec::Output);
        connect_pins(&mut doc, (a, PinId::output(0)), (g, PinId::input(0)));
        connect_pins(&mut doc, (g, PinId::output(0)), (o, PinId::input(0)));
        doc.rebuild_circuit();

        let g_key = doc.components[&g].key;
        let o_key = doc.components[&o].key;
        assert_eq!(doc.circuit.read_output(o_key), Value::ZERO); // NOT(1) = 0
        doc.selected = Some(Selection::Single(Selected::Component(g)));

        doc.delete_component(g);

        // The placed record is genuinely removed (its payload moved into the
        // undo entry), so its key no longer resolves.
        assert!(!doc.components.contains_key(&g));
        // Circuit-side removal also deletes the component outright.
        assert!(!doc.circuit.components.contains_key(&g_key));
        // No wire node references the deleted component; orphan neighbours were
        // cleaned up too, leaving no segments.
        assert!(doc
            .wiring
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Pin(k, _) if k == g)));
        assert_eq!(doc.wiring.active_segments().count(), 0);
        assert_eq!(doc.selected, None);
        // Output's input pin is now Floating.
        assert_eq!(doc.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_tunnel_drops_wire_nodes() {
        // A component pin wired to a tunnel: deleting the tunnel removes its wire
        // nodes and clears the selection.
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let t = doc.place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let t_key = doc.tunnels[&t].key;
        connect_pin_tunnel(&mut doc, (a, PinId::output(0)), t);
        doc.rebuild_circuit();
        doc.selected = Some(Selection::Single(Selected::Tunnel(t)));

        doc.delete_tunnel(t);

        // Placed record genuinely removed (payload moved into the undo entry).
        assert!(!doc.tunnels.contains_key(&t));
        // Circuit-side removal also deletes the tunnel outright.
        assert!(!doc.circuit.tunnels.contains_key(&t_key));
        assert!(doc
            .wiring
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Tunnel(k) if k == t)));
        assert_eq!(doc.selected, None);
    }

    #[test]
    fn test_rebuild_circuit_reconciles_tunnel_labels() {
        // Regression for the tunnel-rename bug: if the GUI's PlacedTunnel.label
        // is changed without a matching circuit.rename_tunnel (e.g. the user
        // committed the rename by clicking away rather than pressing Enter),
        // rebuild_circuit must reconcile the circuit's label from the GUI's, so
        // the renamed Feed/Pull pair form one group and the value propagates.
        let mut doc = Document::blank();
        let inp = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let out = place(&mut doc, ComponentSpec::Output);
        let pull = doc.place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let feed = doc.place_tunnel(TunnelRole::Feed, GridPos::new(2, 2));

        connect_pin_tunnel(&mut doc, (inp, PinId::output(0)), pull);
        connect_pin_tunnel(&mut doc, (out, PinId::input(0)), feed);
        doc.rebuild_circuit();

        let out_key = doc.components[&out].key;
        assert_eq!(doc.circuit.read_output(out_key), Value::Floating);

        // GUI label changed only; circuit.rename_tunnel deliberately NOT called.
        let shared = doc.tunnels[&pull].label.clone();
        doc.tunnels.get_mut(&feed).unwrap().label = shared;
        doc.rebuild_circuit();

        assert_eq!(doc.circuit.read_output(out_key), Value::ONE);
    }

    #[test]
    fn test_bulk_select_box_contains_and_delete() {
        // Two components near the origin and one far away. A box over the origin
        // cluster selects exactly those two; a bulk delete removes them and
        // leaves the far one (and clears the selection).
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let b = place_at(&mut doc, ComponentSpec::Output, GridPos::new(2, 2));
        let far = place_at(&mut doc, ComponentSpec::Output, GridPos::new(50, 50));
        connect_pins(&mut doc, (a, PinId::output(0)), (b, PinId::input(0)));
        doc.rebuild_circuit();

        let camera = Camera::default();
        let rect = selection_screen_rect(GridPos::new(-2, -2), GridPos::new(12, 12), camera);
        let items = doc.items_in_rect(rect, camera);
        assert!(items.contains(&Selected::Component(a)));
        assert!(items.contains(&Selected::Component(b)));
        assert!(!items.contains(&Selected::Component(far)));

        doc.selected = Some(Selection::Bulk(items));
        doc.delete_bulk();

        // Deleted records are tombstoned (inactive), the untouched one stays active.
        assert!(!doc.components.contains_key(&a));
        assert!(!doc.components.contains_key(&b));
        assert!(doc.components.contains_key(&far));
        assert_eq!(doc.selected, None);
        // The wire between a and b went with them.
        assert_eq!(doc.wiring.active_segments().count(), 0);
    }

    #[test]
    fn test_delete_wire_segment_splits_net() {
        // Input -> NOT -> Output as two wires; delete the input->gate wire and
        // the gate's input goes Floating (net split), so the output does too.
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut doc,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        let o = place(&mut doc, ComponentSpec::Output);
        connect_pins(&mut doc, (a, PinId::output(0)), (g, PinId::input(0)));
        connect_pins(&mut doc, (g, PinId::output(0)), (o, PinId::input(0)));
        doc.rebuild_circuit();
        let o_key = doc.components[&o].key;
        assert_eq!(doc.circuit.read_output(o_key), Value::ZERO);

        // Delete the a->g segment (the one touching a's output pin node).
        let seg = doc
            .wiring
            .segments
            .iter()
            .find(|(_, s)| {
                matches!(doc.wiring.nodes[&s.a].attach, NodeAttach::Pin(k, _) if k == a)
                    || matches!(doc.wiring.nodes[&s.b].attach, NodeAttach::Pin(k, _) if k == a)
            })
            .map(|(k, _)| *k)
            .unwrap();
        doc.delete_wire(seg);

        // g's input is now Floating -> NOT(Floating) = Floating at the output.
        assert_eq!(doc.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_wire_records_only_the_wiring_delta() {
        // rebuild_circuit is history-free: its ClearNets/Link net reconstruction
        // is *derived* state that records nothing. So deleting a wire produces
        // exactly one entry - the Gui WiringDelta - with no Sim entries from the
        // relink (which used to pad the batch with RelinkAll + per-link undos).
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut doc,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        connect_pins(&mut doc, (a, PinId::output(0)), (g, PinId::input(0)));
        doc.rebuild_circuit();

        let seg = doc.wiring.active_segments().next().map(|(k, _)| k).unwrap();
        let stack_before = doc.history.len();
        doc.delete_wire(seg);

        assert_eq!(doc.history.len(), stack_before + 1);
        assert!(matches!(
            doc.history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }

    #[test]
    fn test_commit_move_pushes_undo_only_when_position_changed() {
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let original = doc.components[&a].grid_pos;
        let stack_before = doc.history.len();

        // No movement: nothing pushed.
        doc.commit_move(Selected::Component(a), original);
        assert_eq!(doc.history.len(), stack_before);

        // Moved: pushes one MoveComponent entry with the correct old_pos.
        doc.components.get_mut(&a).unwrap().grid_pos = GridPos::new(original.x + 3, original.y + 1);
        doc.commit_move(Selected::Component(a), original);
        assert_eq!(doc.history.len(), stack_before + 1);
        match doc.history.last() {
            Some(HistoryEntry::Gui(GuiUndoAction::MoveComponent { key, old_pos })) => {
                assert_eq!(*key, a);
                assert_eq!(*old_pos, original);
            }
            other => panic!("expected Gui(MoveComponent), got {other:?}"),
        }
    }

    #[test]
    fn test_bulk_move_commits_as_one_undo_batch() {
        let mut doc = Document::blank();
        let a = place_at(&mut doc, ComponentSpec::Output, GridPos::new(0, 0));
        let b = place_at(&mut doc, ComponentSpec::Output, GridPos::new(10, 0));
        let orig_a = doc.components[&a].grid_pos;
        let orig_b = doc.components[&b].grid_pos;
        let stack_before = doc.history.len();

        // Mirrors what interact_component_drag's drag_stopped branch does for
        // a Selection::Bulk: every dragged item already moved (one frame at
        // a time, by the same pointer delta - simulated here directly since
        // driving the gesture needs a live egui::Response), then the whole
        // set is committed inside one begin_batch/end_batch.
        doc.components.get_mut(&a).unwrap().grid_pos = GridPos::new(orig_a.x + 3, orig_a.y + 2);
        doc.components.get_mut(&b).unwrap().grid_pos = GridPos::new(orig_b.x + 3, orig_b.y + 2);

        doc.history.begin_batch();
        doc.commit_move(Selected::Component(a), orig_a);
        doc.commit_move(Selected::Component(b), orig_b);
        doc.history.end_batch();

        // One batch entry for the whole gesture, not one per item.
        assert_eq!(doc.history.len(), stack_before + 1);
        assert!(matches!(doc.history.last(), Some(HistoryEntry::Batch(_))));

        // One undo restores every item's original position at once.
        doc.undo();
        assert_eq!(doc.components[&a].grid_pos, orig_a);
        assert_eq!(doc.components[&b].grid_pos, orig_b);

        // One redo replays the whole move again.
        doc.redo();
        assert_eq!(
            doc.components[&a].grid_pos,
            GridPos::new(orig_a.x + 3, orig_a.y + 2)
        );
        assert_eq!(
            doc.components[&b].grid_pos,
            GridPos::new(orig_b.x + 3, orig_b.y + 2)
        );
    }

    #[test]
    fn test_drag_grid_pos_excludes_wire_selection() {
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Output);
        assert!(doc
            .drag_grid_pos(
                Selected::Wire(crate::gui::wiring::WireSegKey(0)),
                Camera::default()
            )
            .is_none());
        assert!(doc
            .drag_grid_pos(Selected::Component(a), Camera::default())
            .is_some());
    }

    // Builds a two-segment route (pin -> Free elbow -> pin) between two
    // freshly placed components, returning the components, the elbow's
    // WireNodeKey, and both segment keys.
    fn place_route_with_elbow(
        doc: &mut Document,
    ) -> (PlacedCompKey, PlacedCompKey, WireNodeKey, Vec<WireSegKey>) {
        let a = place_at(
            doc,
            ComponentSpec::Input(Input { bits: 1, width: 1 }),
            GridPos::new(0, 0),
        );
        let b = place_at(doc, ComponentSpec::Output, GridPos::new(10, 0));
        let pa = pin_grid_pos(
            &doc.components[&a].shape,
            doc.components[&a].grid_pos,
            PinId::output(0),
        );
        let pb = pin_grid_pos(
            &doc.components[&b].shape,
            doc.components[&b].grid_pos,
            PinId::input(0),
        );
        let elbow = GridPos::new(pa.x + 2, pb.y + 4);
        let delta = doc.wiring.add_route(
            &[pa, elbow, pb],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(b, PinId::input(0)),
        );
        doc.edit_wiring(delta);
        doc.rebuild_circuit();

        let elbow_key = doc
            .wiring
            .active_nodes()
            .find(|(_, n)| matches!(n.attach, NodeAttach::Free))
            .map(|(k, _)| k)
            .unwrap();
        let seg_keys: Vec<WireSegKey> = doc.wiring.active_segments().map(|(k, _)| k).collect();
        (a, b, elbow_key, seg_keys)
    }

    #[test]
    fn test_free_wire_nodes_dedupes_shared_elbow_and_excludes_pin_nodes() {
        let mut doc = Document::blank();
        let (_, _, elbow, segs) = place_route_with_elbow(&mut doc);
        assert_eq!(segs.len(), 2, "pin -> elbow -> pin is two segments");

        let sels: Vec<Selected> = segs.iter().map(|&s| Selected::Wire(s)).collect();
        let free_nodes = doc.free_wire_nodes(&sels);

        // Both segments share the elbow node; it must appear exactly once,
        // and the two Pin-attached endpoints must not appear at all.
        assert_eq!(free_nodes.len(), 1);
        assert_eq!(free_nodes[0].0, elbow);
        assert_eq!(free_nodes[0].1, doc.wiring.nodes[&elbow].pos);
    }

    #[test]
    fn test_bulk_drag_batch_restores_free_wire_node_alongside_components() {
        let mut doc = Document::blank();
        let (a, b, elbow, _segs) = place_route_with_elbow(&mut doc);
        let orig_a = doc.components[&a].grid_pos;
        let orig_b = doc.components[&b].grid_pos;
        let orig_elbow = doc.wiring.nodes[&elbow].pos;

        // What interact_component_drag does for one drag frame of a bulk
        // selection covering both components and the whole wire run: move
        // every component (syncing its pin-attached nodes) and every Free
        // elbow node by the same delta.
        let new_a = GridPos::new(orig_a.x + 3, orig_a.y + 2);
        let new_b = GridPos::new(orig_b.x + 3, orig_b.y + 2);
        let new_elbow = GridPos::new(orig_elbow.x + 3, orig_elbow.y + 2);
        doc.components.get_mut(&a).unwrap().grid_pos = new_a;
        doc.sync_component_wire_nodes(a);
        doc.components.get_mut(&b).unwrap().grid_pos = new_b;
        doc.sync_component_wire_nodes(b);
        doc.wiring.nodes.get_mut(&elbow).unwrap().pos = new_elbow;

        // Wire geometry moved as a whole - the elbow isn't left behind.
        assert_eq!(doc.wiring.nodes[&elbow].pos, new_elbow);

        // drag_stopped: commit every moved item and node as one undo batch.
        let stack_before = doc.history.len();
        doc.history.begin_batch();
        doc.commit_move(Selected::Component(a), orig_a);
        doc.commit_move(Selected::Component(b), orig_b);
        doc.commit_wire_node_move(elbow, orig_elbow);
        doc.history.end_batch();
        assert_eq!(doc.history.len(), stack_before + 1);
        assert!(matches!(doc.history.last(), Some(HistoryEntry::Batch(_))));

        // One undo restores the components AND the elbow together.
        doc.undo();
        assert_eq!(doc.components[&a].grid_pos, orig_a);
        assert_eq!(doc.components[&b].grid_pos, orig_b);
        assert_eq!(doc.wiring.nodes[&elbow].pos, orig_elbow);

        // One redo replays the whole move again.
        doc.redo();
        assert_eq!(doc.components[&a].grid_pos, new_a);
        assert_eq!(doc.components[&b].grid_pos, new_b);
        assert_eq!(doc.wiring.nodes[&elbow].pos, new_elbow);
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
        let mut doc = Document::blank();
        let g = place(&mut doc, and2());
        let comp_key = doc.components[&g].key;
        assert!(doc.history.can_undo());
        assert!(!doc.history.can_redo());
        assert!(doc.components.contains_key(&g));
        assert!(doc.circuit.components.contains_key(&comp_key));

        doc.undo();
        assert!(
            !doc.components.contains_key(&g),
            "record tombstoned by undo"
        );
        assert!(
            !doc.circuit.components.contains_key(&comp_key),
            "circuit component deactivated by undo"
        );
        assert!(doc.history.can_redo());
        assert!(!doc.history.can_undo());

        doc.redo();
        assert!(doc.components.contains_key(&g));
        assert!(doc.circuit.components.contains_key(&comp_key));
        assert!(doc.history.can_undo());
        assert!(!doc.history.can_redo());
    }

    #[test]
    fn undo_redo_wire_draw_round_trips_connectivity() {
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let o = place(&mut doc, ComponentSpec::Output);
        doc.commit_wire_route(
            vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(o, PinId::input(0)),
        );
        let o_key = doc.components[&o].key;
        assert_eq!(doc.circuit.read_output(o_key), Value::ONE);
        assert_eq!(doc.wiring.groups().len(), 1);

        doc.undo();
        assert!(
            doc.wiring.groups().iter().all(|grp| grp.pins.len() < 2),
            "wire removed: no group ties both pins together"
        );
        assert_eq!(doc.circuit.read_output(o_key), Value::Floating);

        doc.redo();
        assert_eq!(doc.wiring.groups().len(), 1);
        assert_eq!(doc.circuit.read_output(o_key), Value::ONE);
    }

    #[test]
    fn undo_redo_delete_component_restores_wire_and_value() {
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let o = place(&mut doc, ComponentSpec::Output);
        doc.commit_wire_route(
            vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(o, PinId::input(0)),
        );
        let o_key = doc.components[&o].key;
        assert_eq!(doc.circuit.read_output(o_key), Value::ONE);

        doc.delete_component(a);
        assert!(!doc.components.contains_key(&a));
        assert_eq!(doc.circuit.read_output(o_key), Value::Floating);

        doc.undo();
        assert!(doc.components.contains_key(&a));
        let a_key = doc.components[&a].key;
        assert!(doc.circuit.components.contains_key(&a_key));
        assert_eq!(
            doc.circuit.read_output(o_key),
            Value::ONE,
            "wire nodes and driving input restored"
        );

        doc.redo();
        assert!(!doc.components.contains_key(&a));
        assert_eq!(doc.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn undo_redo_reconfigure_restores_def_and_key() {
        let mut doc = Document::blank();
        let g = place(&mut doc, and2());
        let old_key = doc.components[&g].key;
        let new_spec = ComponentSpec::Gate(Gate {
            op: GateOp::Not,
            n_inputs: 1,
            width: 1,
        });
        let new_comp = new_spec.to_component();
        doc.reconfigure_component(g, new_spec, new_comp);
        assert!(matches!(
            doc.components[&g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                ..
            })
        ));

        doc.undo();
        assert!(matches!(
            doc.components[&g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::And,
                n_inputs: 2,
                ..
            })
        ));
        assert_eq!(doc.components[&g].key, old_key, "old CompKey restored");

        doc.redo();
        assert!(matches!(
            doc.components[&g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                ..
            })
        ));
    }

    #[test]
    fn undo_redo_move_restores_grid_pos() {
        let mut doc = Document::blank();
        let a = place(&mut doc, and2());
        let original = doc.components[&a].grid_pos;
        let moved = GridPos::new(original.x + 4, original.y + 2);
        doc.components.get_mut(&a).unwrap().grid_pos = moved;
        doc.commit_move(Selected::Component(a), original);

        doc.undo();
        assert_eq!(doc.components[&a].grid_pos, original);

        doc.redo();
        assert_eq!(doc.components[&a].grid_pos, moved);
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let mut doc = Document::blank();
        place(&mut doc, and2());
        doc.undo();
        assert!(doc.history.can_redo());
        // A fresh edit invalidates the redo branch.
        place(&mut doc, ComponentSpec::Output);
        assert!(!doc.history.can_redo());
    }

    #[test]
    fn undo_redo_tunnel_rename_restores_label() {
        let mut doc = Document::blank();
        let t = doc.place_tunnel(TunnelRole::Feed, GridPos::new(0, 0));
        let tunnel_key = doc.tunnels[&t].key;
        let original = doc.tunnels[&t].label.clone();

        // Simulate a rename commit: record label change live, then the batched
        // Sim rename + record-label undo (mirrors show_tunnel_properties).
        doc.tunnels.get_mut(&t).unwrap().label = "RENAMED".to_string();
        doc.history.begin_batch();
        doc.apply(Command::RenameTunnel {
            tunnel: tunnel_key,
            new_label: "RENAMED".to_string(),
        });
        doc.history.push_gui(GuiUndoAction::SetTunnelLabel {
            key: t,
            label: original.clone(),
        });
        doc.history.end_batch();

        doc.undo();
        assert_eq!(doc.tunnels[&t].label, original);
        assert_eq!(doc.circuit.tunnels[&tunnel_key].label, original);

        doc.redo();
        assert_eq!(doc.tunnels[&t].label, "RENAMED");
        assert_eq!(doc.circuit.tunnels[&tunnel_key].label, "RENAMED");
    }

    // ── clock lock predicates ─────────────────────────────────────────────

    #[test]
    fn test_editing_locked_tracks_run_state() {
        let mut doc = Document::blank();
        // Stopped (initial) is editable.
        assert!(!doc.editing_locked());

        // Both Playing and Paused lock the whole run session.
        doc.clock.run = ClockRun::Playing;
        assert!(doc.editing_locked());
        doc.clock.run = ClockRun::Paused;
        assert!(doc.editing_locked());

        // Stop returns to editable.
        doc.stop_clock();
        assert_eq!(doc.clock.run, ClockRun::Stopped);
        assert!(!doc.editing_locked());
    }

    #[test]
    fn test_value_editing_locked_only_while_playing() {
        let mut doc = Document::blank();
        // Stopped: fully editable, so value edits are allowed.
        assert!(!doc.value_editing_locked());

        // Paused carves value edits (Input bits, ROM/RAM contents) out of the
        // structural lock: still not value-locked, even though editing_locked().
        doc.clock.run = ClockRun::Paused;
        assert!(doc.editing_locked());
        assert!(!doc.value_editing_locked());

        // Playing blocks everything, values included.
        doc.clock.run = ClockRun::Playing;
        assert!(doc.value_editing_locked());

        doc.stop_clock();
        assert!(!doc.value_editing_locked());
    }

    #[test]
    fn test_stop_clock_resets_register_through_gui() {
        let mut doc = Document::blank();
        let data = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let we = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let reg = place(&mut doc, ComponentSpec::Reg(RegConf { data_width: 1 }));
        let out = place(&mut doc, ComponentSpec::Output);

        connect_pins(&mut doc, (data, PinId::output(0)), (reg, PinId::input(0)));
        connect_pins(&mut doc, (we, PinId::output(0)), (reg, PinId::input(1)));
        connect_pins(&mut doc, (reg, PinId::output(0)), (out, PinId::input(0)));
        doc.rebuild_circuit();

        let out_key = doc.components[&out].key;
        // Register powers on at 0.
        assert_eq!(doc.circuit.read_output(out_key), Value::ZERO);

        // A tick with write-enable high latches the data (1).
        doc.tick_once();
        assert_eq!(doc.circuit.read_output(out_key), Value::ONE);

        // Stop resets the register to 0 and returns to the editable state.
        doc.clock.run = ClockRun::Playing;
        doc.stop_clock();
        assert_eq!(doc.clock.run, ClockRun::Stopped);
        assert_eq!(doc.circuit.read_output(out_key), Value::ZERO);
    }

    #[test]
    fn undo_redo_delete_register_preserves_latched_state() {
        // The move-based model: deleting a register moves its live Component
        // (latch and all) into the undo entry, so undo restores the exact
        // latched value - what a spec-based re-creation would lose.
        let mut doc = Document::blank();
        let data = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let we = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let reg = place(&mut doc, ComponentSpec::Reg(RegConf { data_width: 1 }));
        let out = place(&mut doc, ComponentSpec::Output);
        connect_pins(&mut doc, (data, PinId::output(0)), (reg, PinId::input(0)));
        connect_pins(&mut doc, (we, PinId::output(0)), (reg, PinId::input(1)));
        connect_pins(&mut doc, (reg, PinId::output(0)), (out, PinId::input(0)));
        doc.rebuild_circuit();

        let out_key = doc.components[&out].key;
        doc.tick_once(); // latch 1 into the register
        assert_eq!(doc.circuit.read_output(out_key), Value::ONE);

        // Delete the register: its record and circuit component are gone.
        doc.delete_component(reg);
        assert!(!doc.components.contains_key(&reg));

        // Undo restores it under the same PlacedCompKey with its latch intact:
        // the output reads 1 again with no re-tick.
        doc.undo();
        assert!(doc.components.contains_key(&reg));
        assert_eq!(
            doc.circuit.read_output(out_key),
            Value::ONE,
            "moved-in Component carried the latched 1 back"
        );

        // Redo removes it again.
        doc.redo();
        assert!(!doc.components.contains_key(&reg));
    }
}

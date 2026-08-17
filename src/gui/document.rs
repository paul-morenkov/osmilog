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
use crate::gui::signal_viewer::{SignalLog, SignalViewer};
use crate::gui::theme::Theme;
use crate::gui::utils::CanvasCtx;
use crate::gui::wiring::{NodeAttach, WireNodeKey, WireSegKey, Wiring};
use crate::sim::circuit::{Circuit, TunnelKey, TunnelRole};
use crate::sim::command::{Command, CommandOutput};
use crate::sim::component::{CompKey, Component, ComponentSpec, PinId};
use crate::sim::value::Value;

/// Defined in `sim::component` so `ComponentSpec::Subcircuit` can embed it without a gui
/// dependency.
pub use crate::sim::component::DocId;

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

/// Only a suggestion, prefilled into the dialog. Names need not be unique; identity is the
/// `DocId`.
pub(crate) fn default_new_circuit_name(documents: &SlotMap<DocId, CircuitDoc>) -> String {
    format!("Circuit {}", documents.len() + 1)
}
/// Every document holds one of these directly; there is no separate "live" copy the active
/// document promotes into.
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
    // Runtime-only probe history + viewer UI state; never saved.
    pub(crate) signal_log: SignalLog,
    pub(crate) signal_viewer: SignalViewer,
    pub(crate) memory_editor: MemoryEditor,
    // Distinct from OsmilogApp::io_error; the menu bar shows I/O errors first.
    pub(crate) settle_error: Option<String>,
}

impl Document {
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
            signal_log: SignalLog::default(),
            signal_viewer: SignalViewer::default(),
            memory_editor: MemoryEditor::default(),
            settle_error: None,
        }
    }

    // True while a clock run is active (Playing or Paused); structural edits
    // stay locked until Stop.
    pub(crate) fn editing_locked(&self) -> bool {
        self.clock.run != ClockRun::Stopped
    }

    // True only while Playing; a Paused run still allows value pokes (Input
    // bits, ROM/RAM contents). See OsmilogApp::value_editing_locked.
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

    // Untracked: never lands on the undo stack (see Clock::step). Records one
    // probe sample on a successful tick.
    pub(crate) fn tick_once(&mut self) {
        let result = self.clock.step(&mut self.circuit);
        let ok = result.is_ok();
        self.record_settle_result(result);
        if ok {
            self.sample_probes();
        }
    }

    // Resets all sequential state to its power-on value (see Clock::stop) and
    // clears the probe history so the next run starts clean.
    pub(crate) fn stop_clock(&mut self) {
        let result = self.clock.stop(&mut self.circuit);
        self.record_settle_result(result);
        self.signal_log.clear();
    }

    // Bridges the frame clock (wasm-safe) into the cadence math on Clock, then
    // fires and samples each due tick here so every tick lands in the history
    // (Clock::poll may report several ticks in one late frame). Auto-pauses on a
    // settle failure so we don't hammer a broken circuit every frame.
    pub(crate) fn advance_clock(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let n_ticks = self.clock.poll(now, |wait| {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        });
        for _ in 0..n_ticks {
            let result = self.clock.step(&mut self.circuit);
            let failed = result.is_err();
            self.record_settle_result(result);
            if failed {
                self.clock.pause();
                return;
            }
            self.sample_probes();
        }
    }

    // Appends the current value of every placed Probe to the signal log, one
    // sample per clock tick. Reads three disjoint Document fields.
    pub(crate) fn sample_probes(&mut self) {
        let samples: Vec<(PlacedCompKey, Value)> = self
            .components
            .iter()
            .filter(|(_, pc)| matches!(pc.spec, ComponentSpec::Probe(_)))
            .map(|(&k, pc)| (k, self.circuit.read_output(pc.key)))
            .collect();
        self.signal_log.record(&samples);
    }

    // A suggested unique-ish name for a newly placed probe. Names need not be
    // unique (like tunnels), so this is only a convenience default.
    pub(crate) fn next_probe_name(&self) -> String {
        let n = self
            .components
            .values()
            .filter(|pc| matches!(pc.spec, ComponentSpec::Probe(_)))
            .count();
        format!("probe{}", n + 1)
    }

    // Like circuit.apply(), but also records the UndoAction into history.
    pub(crate) fn apply(&mut self, command: Command) -> CommandOutput {
        let (output, undo) = self.circuit.apply(command);
        self.history.push_sim(undo);
        output
    }

    // ── Undo / redo ───────────────────────────────────────────────────────────

    // Reverses one entry, returning its inverse for the opposite stack. A Batch
    // applies child-last-first, so the inverses replay in original order.
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

    pub(crate) fn undo(&mut self) {
        if let Some(entry) = self.history.pop_undo() {
            let inverse = self.apply_entry(entry);
            self.history.push_redo(inverse);
            self.refresh_after_history();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(entry) = self.history.pop_redo() {
            let inverse = self.apply_entry(entry);
            self.history.push_undo(inverse);
            self.refresh_after_history();
        }
    }

    // Resyncs wire-node geometry (MoveComponent carries no wiring delta),
    // clears stale selection, and rebuilds nets.
    fn refresh_after_history(&mut self) {
        let comp_keys: Vec<_> = self.components.keys().copied().collect();
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

    // comp is built by OsmilogApp::instantiate (needs the document registry
    // for subcircuits); this only does per-document bookkeeping.
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
        // Undo removes this record; paired with the Sim RemoveComponent already
        // recorded above.
        self.history
            .push_gui(GuiUndoAction::RemoveComponent { key: pc_key });
        self.history.end_batch();
        pc_key
    }

    // PlacedCompKey stays stable so wires survive; only pins the new arity
    // drops lose their wire nodes. new_comp is built by the caller via
    // instantiate (needs the document registry).
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
        // Undo restores the old CompKey + spec; the Sim actions above only
        // handle the circuit component.
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

    // Shared by place_tunnel (auto label) and install_circuit_records (saved
    // label) - tunnels connect by matching label.
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
        // Undo removes this record; paired with the Sim RemoveTunnel recorded
        // above.
        self.history
            .push_gui(GuiUndoAction::RemoveTunnel { key: pt_key });
        self.history.end_batch();
        pt_key
    }

    // Live tunnels only, mirroring Wiring::active_nodes/active_segments.
    pub(crate) fn active_tunnels(&self) -> impl Iterator<Item = (PlacedTunnelKey, &PlacedTunnel)> {
        self.tunnels.iter().map(|(k, pt)| (*k, pt))
    }

    // Rebuilds nets from the wiring graph; fan-out/driver-conflict handling
    // lives in Circuit::link.
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

    // Attached segments stretch automatically; only the anchor nodes move here.
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

    // All nodes in a connected group share one net; the group's value comes
    // from any pin/tunnel endpoint on it.
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

        // Colour comes from the group's net value; a dangling group (no
        // endpoints) is Floating.
        let node_value = self.wire_node_values();

        for (seg_key, seg) in &self.wiring.segments {
            let a = self.wiring.nodes[&seg.a];
            let b = self.wiring.nodes[&seg.b];
            let p0 = camera.grid_to_screen(a.pos);
            let p1 = camera.grid_to_screen(b.pos);
            let val = node_value.get(&seg.a).copied().unwrap_or(Value::Floating);
            let mut stroke = value_stroke(theme, val);
            if self.is_highlighted(Selected::Wire(*seg_key)) {
                stroke.color = theme.outline_selected;
                stroke.width += 1.5;
            }
            stroke.width = camera.scale(stroke.width);
            let (p0, p1) = extend_segment(p0, p1, stroke.width / 2.0);
            painter.line_segment([p0, p1], stroke);
        }

        // Junction dots mark 3+-way branches, distinct from a mere crossing.
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

        for (&pc_key, pc) in &self.components {
            let is_selected = self.is_highlighted(Selected::Component(pc_key));
            draw_component(painter, pc, camera, &self.circuit, is_selected, theme);
        }

        for (pt_key, pt) in self.active_tunnels() {
            let is_selected = self.is_highlighted(Selected::Tunnel(pt_key));
            draw_tunnel(painter, pt, camera, &self.circuit, is_selected, theme);
        }
    }

    // Priority: output pin, input pin, tunnel, wire node, wire segment, else
    // the snapped cursor cell.
    pub(crate) fn wire_target_at(&self, pos: Pos2, camera: Camera) -> (NodeAttach, GridPos, bool) {
        puffin::profile_function!();
        if let Some((pck, pin)) = pin_at_pos(self.components.iter(), camera, pos, PinKind::Output) {
            let gp = pin_grid_pos(
                &self.components[&pck].shape,
                self.components[&pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some((pck, pin)) = pin_at_pos(self.components.iter(), camera, pos, PinKind::Input) {
            let gp = pin_grid_pos(
                &self.components[&pck].shape,
                self.components[&pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some(ptk) = tunnel_pin_at_pos(self.tunnels.iter(), camera, pos) {
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

    // Spec and live component share one buffer (Rom::shared), so this updates
    // both. Not undoable, like a clock tick.
    pub(crate) fn write_rom_cell(&mut self, pc: PlacedCompKey, index: usize, value: u32) {
        let comp_key = self.components[&pc].key;
        self.circuit.write_rom(comp_key, index, value);
        let result = self.circuit.settle();
        self.record_settle_result(result);
    }

    // No settle() needed: RAM's data_out is a registered output, only updated
    // by tick_clock. Not undoable, like a ROM's.
    pub(crate) fn write_ram_cell(&mut self, pc: PlacedCompKey, index: usize, value: u32) {
        let comp_key = self.components[&pc].key;
        self.circuit.write_ram(comp_key, index, value);
    }

    // Applying stays here: the write paths need &mut Circuit + settle, which
    // MemoryEditor doesn't own.
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

    pub(crate) fn delete_component(&mut self, key: PlacedCompKey) {
        self.history.begin_batch();
        let comp_key = self.components[&key].key;
        self.apply(Command::RemoveComponent(comp_key));
        let delta = self.wiring.remove_component_nodes(key);
        self.edit_wiring(delta);
        // Undo re-inserts this record under the same PlacedCompKey (see
        // apply_gui_undo).
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

    // The wiring graph handles orphan cleanup and any net split.
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

    // True if `sel` is the single selection or part of a bulk selection.
    pub(crate) fn is_highlighted(&self, sel: Selected) -> bool {
        match &self.selected {
            Some(Selection::Single(s)) => *s == sel,
            Some(Selection::Bulk(items)) => items.contains(&sel),
            None => false,
        }
    }

    // `None` for a Selected::Wire (no draggable body) or a stale key.
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

    // Deduped Free-attached WireNodes only; Pin/Tunnel-attached nodes follow
    // their owner via sync_*_wire_nodes instead.
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

    // A component/tunnel counts when its bounding rect is inside `rect`; a
    // wire counts when both endpoints are.
    pub(crate) fn items_in_rect(&self, rect: Rect, camera: Camera) -> Vec<Selected> {
        puffin::profile_function!();
        let mut out = Vec::new();
        for (&key, pc) in &self.components {
            if rect.contains_rect(component_bounding_rect(pc, camera)) {
                out.push(Selected::Component(key));
            }
        }
        for (key, pt) in self.active_tunnels() {
            if rect.contains_rect(tunnel_bounding_rect(pt, camera)) {
                out.push(Selected::Tunnel(key));
            }
        }
        for (key, seg) in &self.wiring.segments {
            let a = camera.grid_to_screen(self.wiring.nodes[&seg.a].pos);
            let b = camera.grid_to_screen(self.wiring.nodes[&seg.b].pos);
            if rect.contains(a) && rect.contains(b) {
                out.push(Selected::Wire(*key));
            }
        }
        out
    }

    // Wire segments first, then components/tunnels; each removal is
    // existence-checked since deleting a component can take a wire in the
    // same set with it.
    // TODO: slow - each remove_component_nodes call loops unnecessarily. Aggregate the nodes to
    // remove into one pass instead.
    pub(crate) fn delete_bulk(&mut self) {
        let Some(Selection::Bulk(items)) = self.selected.take() else {
            return;
        };
        self.history.begin_batch();
        for sel in &items {
            if let Selected::Wire(seg) = *sel {
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

    // ── Canvas mode dispatch ─────────────────────────────────────────────────
    //
    // One method per InteractionMode variant (see
    // OsmilogApp::handle_canvas_interaction). Placing stays on OsmilogApp since
    // it needs instantiate (subcircuits); everything else lives here.
    pub(crate) fn interact_idle(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>) {
        puffin::profile_function!();
        let locked = self.editing_locked();

        // Hover reticle: hovering over a wire (but not a pin) shows
        // where a branch would tap the wire.
        if let Some(pos) = pointer {
            if pin_at_pos(self.components.iter(), cc.camera, pos, PinKind::Output).is_none()
                && pin_at_pos(self.components.iter(), cc.camera, pos, PinKind::Input).is_none()
                && tunnel_pin_at_pos(self.tunnels.iter(), cc.camera, pos).is_none()
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

        // Suppressed during a run session; click-to-select stays available.
        // Middle-button drags pan the camera instead (handle_camera_input) and
        // must not also start an edit gesture.
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
                        // Move it, but only if the drag began inside its
                        // bounding rect. A selected wire has no body to drag
                        // (drag_grid_pos returns None).
                        let sel = *sel;
                        self.drag_grid_pos(sel, cc.camera)
                            .filter(|(rect, _)| rect.contains(pos))
                            .map(|(_, grid_pos)| (vec![(sel, grid_pos)], Vec::new()))
                    }
                    Some(Selection::Bulk(sels)) => {
                        // Move every selected item together, plus any covered
                        // Free wire node, if the drag began inside any one
                        // item's bounding rect.
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
                // A click starts a polyline only from a pin/tunnel; a bare
                // wire click selects it instead (branching off a wire is a
                // drag, handled above).
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
                    // Priority: component, then tunnel, then wire, else deselect.
                    let maybe_comp = self
                        .components
                        .iter()
                        .find(|(_, pc)| component_bounding_rect(pc, cc.camera).contains(pos))
                        .map(|(k, _)| Selected::Component(*k));
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

        if cc.response.double_clicked() {
            if let Some(pos) = pointer {
                // Same guard intent as single-click priority: ignore if over a component.
                let on_component = self
                    .components
                    .iter()
                    .any(|(_, pc)| component_bounding_rect(pc, cc.camera).contains(pos));
                if !on_component {
                    if let Some((seg, _)) = self.wiring.segment_at_pos(pos, cc.camera) {
                        let segs = self.wiring.net_segments(seg);
                        self.selected = match segs.len() {
                            0 => None,
                            1 => Some(Selection::Single(Selected::Wire(segs[0]))),
                            _ => Some(Selection::Bulk(
                                segs.into_iter().map(Selected::Wire).collect(),
                            )),
                        };
                    }
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
            // Every item moves by the same delta, so bulk drags keep relative
            // layout intact.
            for &(key, original_grid_pos) in &items {
                let new_grid_pos =
                    GridPos::new(original_grid_pos.x + delta_x, original_grid_pos.y + delta_y);
                // Wire-anchor nodes move with the item; attached segments
                // stretch. Topology is unchanged, so no rebuild is needed.
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
            // Free-attached nodes have no owner to carry them along, so they
            // move by the same delta directly, or an interior corner would
            // stay pinned while its ends move.
            for &(key, original_pos) in &free_nodes {
                self.wiring.nodes.get_mut(&key).unwrap().pos =
                    GridPos::new(original_pos.x + delta_x, original_pos.y + delta_y);
            }
        }
        if cc.response.drag_stopped() {
            // One undo batch restores every original position at once, even
            // if some items never moved (pointer stayed in one grid cell).
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

        // The `!dragged` guard recovers from a flick released the same frame
        // it started (drag_stopped never fires then), so the mode can't stick.
        if cc.response.drag_stopped() || !cc.response.dragged() {
            let selected_items = self.items_in_rect(rect, cc.camera);
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

// Normalized so either drag direction yields the same box.
pub(crate) fn selection_screen_rect(start: GridPos, current: GridPos, camera: Camera) -> Rect {
    Rect::from_two_pos(camera.grid_to_screen(start), camera.grid_to_screen(current))
}

// One horizontal then vertical run from `from` to `to`; returns the corner
// (if any) and `to`, not `from`.
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
        // NOT gate wired Input -> Output; deleting it must clean up nodes,
        // selection, and downstream state.
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

        // Payload moved into the undo entry; the key no longer resolves.
        assert!(!doc.components.contains_key(&g));
        assert!(!doc.circuit.components.contains_key(&g_key));
        // No wire node references the deleted component; orphaned neighbours
        // are gone too.
        assert!(doc
            .wiring
            .nodes
            .values()
            .all(|n| !matches!(n.attach, NodeAttach::Pin(k, _) if k == g)));
        assert_eq!(doc.wiring.segments.len(), 0);
        assert_eq!(doc.selected, None);
        // Output's input pin is now Floating.
        assert_eq!(doc.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_tunnel_drops_wire_nodes() {
        let mut doc = Document::blank();
        let a = place(&mut doc, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let t = doc.place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let t_key = doc.tunnels[&t].key;
        connect_pin_tunnel(&mut doc, (a, PinId::output(0)), t);
        doc.rebuild_circuit();
        doc.selected = Some(Selection::Single(Selected::Tunnel(t)));

        doc.delete_tunnel(t);

        assert!(!doc.tunnels.contains_key(&t));
        assert!(!doc.circuit.tunnels.contains_key(&t_key));
        assert!(doc
            .wiring
            .nodes
            .values()
            .all(|n| !matches!(n.attach, NodeAttach::Tunnel(k) if k == t)));
        assert_eq!(doc.selected, None);
    }

    #[test]
    fn test_rebuild_circuit_reconciles_tunnel_labels() {
        // Regression: if PlacedTunnel.label changes without a matching
        // circuit.rename_tunnel (e.g. rename committed by clicking away),
        // rebuild_circuit must reconcile the label so Feed/Pull still link.
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
        assert_eq!(doc.wiring.segments.len(), 0);
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

        let seg = doc.wiring.segments.keys().next().unwrap();
        let stack_before = doc.history.len();
        doc.delete_wire(*seg);

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

        // Mirrors interact_component_drag's drag_stopped branch: items already
        // moved (simulated directly, since driving the gesture needs a live
        // egui::Response), then commit as one batch.
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

    // Builds a pin -> Free elbow -> pin route between two fresh components.
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
            .nodes
            .iter()
            .find(|(_, n)| matches!(n.attach, NodeAttach::Free))
            .map(|(k, _)| *k)
            .unwrap();
        let seg_keys: Vec<WireSegKey> = doc.wiring.segments.keys().cloned().collect();
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

        // Mirrors one drag frame of a bulk selection: move every component
        // (syncing pin nodes) and every Free elbow node by the same delta.
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

        // Mirrors show_tunnel_properties: live label change, then batched Sim
        // rename + record undo.
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

        // Paused carves value edits out of the structural lock: not
        // value-locked even though editing_locked() is true.
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
        // Deleting a register moves its live Component into the undo entry, so
        // undo restores the exact latched value a spec-based re-creation would lose.
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

        // Undo restores it under the same PlacedCompKey with its latch intact,
        // no re-tick needed.
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

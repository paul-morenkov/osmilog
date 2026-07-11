use eframe;
use egui::epaint::{PathShape, PathStroke};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use slotmap::{new_key_type, SlotMap};
use std::collections::HashMap;

use crate::gui::clipboard::Clipboard;
use crate::gui::geometry::{snap_to_grid, tunnel_shape, GridPos, GRID_SIZE, LABEL_FONT_SIZE};
use crate::gui::gui_undo::GuiUndoAction;
use crate::gui::history::{History, HistoryEntry};
use crate::gui::placed_component::PlacedComponent;
use crate::gui::shape::{tessellate_path, ComponentShape, BUBBLE_R};
use crate::gui::theme::Theme;
use crate::gui::wiring::{NodeAttach, WireNode, WireNodeKey, WireSegKey, WireSegment, Wiring};
use crate::io::{
    CircuitFile, ComponentEntry, LoadError, NodeAttachEntry, NodeEntry, SegEntry, TunnelEntry,
};
use crate::sim::circuit::{Circuit, TunnelKey, TunnelRole};
use crate::sim::command::Command;
use crate::sim::component::{
    Adder, CompKey, Comparator, ComponentSpec, CounterConf, DFlipFlopConf, Demux, Divider, Encoder,
    FanDirection, Gate, GateOp, InIdx, Input, JKFlipFlopConf, Multiplier, Mux, OutIdx,
    OverflowAction, PinId, RegConf, SRFlipFlopConf, Subtractor, TFlipFlopConf,
};
use crate::sim::value::Value;

// ── Constants ─────────────────────────────────────────────────────────────────

const PIN_RADIUS: f32 = 3.0;
const WIRE_THICKNESS_THIN: f32 = 2.0;
const WIRE_THICKNESS_THICK: f32 = 4.0;
const COMP_STROKE: f32 = 1.5;

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

enum PinKind {
    Input,
    Output,
}

// ── ClockControl ──────────────────────────────────────────────────────────────

// The clock transport's run state. Editing is locked whenever this is not
// `Stopped` (see OsmilogApp::editing_locked) - the whole run session (Play →
// Pause → …) is read-only, and only Stop returns to an editable circuit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockRun {
    // Idle and editable; the clock is not advancing (initial state).
    Stopped,
    // Auto-advancing at `ticks_per_second`; editing locked.
    Playing,
    // Frozen mid-run with sequential state preserved; editing locked.
    Paused,
}

// Clock transport state: the run mode plus the auto-advance speed and the
// egui frame-clock timestamp of the last auto-tick. See OsmilogApp::logic for
// the auto-advance loop and show_menu_bar for the Play/Pause/Step/Stop UI.
pub struct ClockControl {
    run: ClockRun,
    // Auto-advance rate in ticks per real second (Playing only).
    ticks_per_second: f32,
    // ctx.input(|i| i.time) value when the last auto-tick fired. Chosen over
    // std::time::Instant, which panics on wasm32.
    last_tick_time: f64,
}

impl Default for ClockControl {
    fn default() -> Self {
        Self {
            run: ClockRun::Stopped,
            ticks_per_second: 1.0,
            last_tick_time: 0.0,
        }
    }
}

// Decides, for one frame of the auto-advance loop, whether the clock should
// tick and what the new `last_tick_time` reference becomes. Returns None when
// the interval hasn't elapsed yet. Kept pure (no egui/self) so the cadence is
// unit-testable.
//
// The reference advances by exactly one `interval` rather than snapping to
// `now`, so the average tick rate stays `1/interval` no matter how often frames
// are produced. (Snapping to `now` folds each frame's overshoot past the
// boundary into the cadence, which is why moving the mouse - a flood of extra
// repaints - visibly sped ticking up.) If we've fallen more than one interval
// behind (the app was idle or suspended), resync to `now` instead of firing a
// catch-up burst of missed ticks.
fn next_tick_schedule(now: f64, last: f64, interval: f64) -> Option<f64> {
    if now - last < interval {
        None
    } else if now - last >= 2.0 * interval {
        Some(now)
    } else {
        Some(last + interval)
    }
}

// ── OsmilogApp ────────────────────────────────────────────────────────────────

new_key_type! {
    pub struct PlacedCompKey;
    pub struct PlacedTunnelKey;
}

pub struct OsmilogApp {
    pub circuit: Circuit,
    // Accumulates UndoActions from every circuit mutation issued via
    // OsmilogApp::apply(). Track-only for now - nothing consumes it yet.
    pub history: History,
    pub components: SlotMap<PlacedCompKey, PlacedComponent>,
    pub tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel>,
    // GUI wiring: the source of truth for connectivity. After any edit the
    // circuit's nets are rebuilt from this graph (see rebuild_circuit).
    pub wiring: Wiring,
    pub mode: InteractionMode,
    pub pan: Vec2,
    // None: nothing selected. Some(Single): properties panel/body-drag target.
    // Some(Bulk): non-empty means Backspace/Delete removes the whole set.
    // Cleared by a click in empty space or Escape.
    pub selected: Option<Selection>,
    // Snapshot of the last copied selection, decoupled from live SlotMap
    // keys so it survives undo/redo and further edits to the copied
    // originals. See gui::clipboard::Clipboard.
    pub clipboard: Clipboard,
    // Also surfaces File > Save/Load I/O errors, not just settle() errors -
    // both are transient "something went wrong" status shown in the same
    // red label in the menu bar (see the "Menu bar" section of `ui`).
    pub last_settle_error: Option<String>,
    // WASM file dialogs are Promise-based, so a load kicked off from the File
    // menu delivers its result here on a later frame, not synchronously - see
    // `apply_pending_load`.
    #[cfg(target_arch = "wasm32")]
    pending_load: crate::io::wasm::PendingLoad,
    // Toggles the in-app puffin flamegraph window (Debug menu). puffin
    // scopes are recorded regardless; this just controls whether the
    // viewer is drawn.
    show_profiler: bool,
    // Clock transport: run state (play/pause/stop), speed, and auto-advance
    // timing. Drives the menu-bar controls and the edit lockout during a run.
    pub clock: ClockControl,
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
    pan: Vec2,
    theme: Theme,
}

impl OsmilogApp {
    // Split out from `new` so tests (and `load_circuit_file`) can construct
    // a fresh app without an eframe::CreationContext, which isn't
    // constructible outside a running eframe host.
    pub fn empty() -> Self {
        Self {
            circuit: Circuit::new(),
            history: History::default(),
            components: SlotMap::default(),
            tunnels: SlotMap::default(),
            wiring: Wiring::new(),
            mode: InteractionMode::Idle,
            pan: Vec2::ZERO,
            selected: None,
            clipboard: Clipboard::new(),
            last_settle_error: None,
            #[cfg(target_arch = "wasm32")]
            pending_load: crate::io::wasm::new_pending_load(),
            show_profiler: false,
            clock: ClockControl::default(),
        }
    }

    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        puffin::set_scopes_on(true);
        Self::empty()
    }

    fn record_settle_result<T>(&mut self, result: Result<T, crate::sim::circuit::SettleError>) {
        match result {
            Ok(_) => self.last_settle_error = None,
            Err(e) => self.last_settle_error = Some(e.to_string()),
        }
    }

    // True while a clock run session is active (Playing or Paused). The single
    // gate for the edit lockout: canvas interaction, shortcuts, the Add/Edit
    // menus, File > Load, and the properties panel are all disabled when this
    // is true. Only Stop (which resets sequential state) returns to editable.
    pub fn editing_locked(&self) -> bool {
        self.clock.run != ClockRun::Stopped
    }

    // Advances the clock exactly one tick, untracked (bypassing self.apply) so
    // it never lands on the undo stack - clock stepping is a simulation step,
    // not a structural edit. Used by both the Step button and the auto-advance
    // loop in logic().
    fn tick_once(&mut self) {
        let result = self.circuit.apply(Command::TickClock).0.unwrap_settle();
        self.record_settle_result(result);
    }

    // Stops the clock: resets all sequential state to its power-on value
    // (untracked, like a tick) and returns to the editable Stopped state.
    pub fn stop_clock(&mut self) {
        let result = self
            .circuit
            .apply(Command::ResetSequential)
            .0
            .unwrap_settle();
        self.record_settle_result(result);
        self.clock.run = ClockRun::Stopped;
    }

    // Auto-advances the clock while Playing. Uses egui's frame clock
    // (ctx.input(|i| i.time), wasm-safe) to fire a tick once per interval, and
    // request_repaint_after to keep the frame loop alive between ticks (the app
    // is otherwise reactive and wouldn't repaint on its own). A tick that fails
    // to settle auto-pauses so we don't hammer a broken circuit every frame.
    fn advance_clock(&mut self, ctx: &egui::Context) {
        if self.clock.run != ClockRun::Playing {
            return;
        }
        let interval = 1.0 / self.clock.ticks_per_second.max(0.001) as f64;
        let now = ctx.input(|i| i.time);
        if let Some(next) = next_tick_schedule(now, self.clock.last_tick_time, interval) {
            self.tick_once();
            self.clock.last_tick_time = next;
            if self.last_settle_error.is_some() {
                self.clock.run = ClockRun::Paused;
                return;
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(interval));
    }

    // Applies a Command and records its UndoAction into history in one place;
    // callers use it exactly like circuit.apply() (same CommandOutput/unwrap_*
    // chaining) without touching the undo data themselves.
    fn apply(&mut self, command: Command) -> crate::sim::command::CommandOutput {
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

    fn place_component(&mut self, spec: ComponentSpec, grid_pos: GridPos) -> PlacedCompKey {
        self.history.begin_batch();
        let comp = spec.to_component();
        let key = self.apply(Command::AddComponent(comp)).unwrap_comp();
        let pc = PlacedComponent {
            key,
            spec,
            grid_pos,
            active: true,
        };
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

    fn place_tunnel(&mut self, role: TunnelRole, grid_pos: GridPos) -> PlacedTunnelKey {
        let label = format!("Tunnel{}", self.tunnels.len());
        self.place_tunnel_labeled(label, role, grid_pos)
    }

    // Shared by place_tunnel (auto-generated label) and load_circuit_file
    // (label restored from a saved file - tunnels connect to each other by
    // matching label, so a loaded tunnel must keep its exact saved label).
    fn place_tunnel_labeled(
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
    fn active_components(&self) -> impl Iterator<Item = (PlacedCompKey, &PlacedComponent)> {
        self.components.iter().filter(|(_, pc)| pc.active)
    }

    fn active_tunnels(&self) -> impl Iterator<Item = (PlacedTunnelKey, &PlacedTunnel)> {
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
    fn sync_component_wire_nodes(&mut self, pck: PlacedCompKey) {
        let Some(pc) = self.components.get(pck) else {
            return;
        };
        let shape = pc.spec.shape();
        let grid_pos = pc.grid_pos;
        self.wiring
            .sync_component_nodes(pck, |pin| pin_grid_pos(&shape, grid_pos, pin));
    }

    fn sync_tunnel_wire_nodes(&mut self, ptk: PlacedTunnelKey) {
        let Some(pt) = self.tunnels.get(ptk) else {
            return;
        };
        self.wiring.sync_tunnel_nodes(ptk, tunnel_pin_grid(pt));
    }

    // The resolved circuit Value at each wire node, for colouring segments. All
    // nodes in a connected group share one net, so we resolve the group's value
    // from any pin/tunnel endpoint on it (Floating if it has none).
    fn wire_node_values(&self) -> HashMap<WireNodeKey, Value> {
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
    fn wire_target_at(&self, pos: Pos2, pan: Vec2) -> (NodeAttach, GridPos, bool) {
        puffin::profile_function!();
        if let Some((pck, pin)) = pin_at_pos(self.active_components(), pan, pos, PinKind::Output) {
            let gp = pin_grid_pos(
                &self.components[pck].spec.shape(),
                self.components[pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some((pck, pin)) = pin_at_pos(self.active_components(), pan, pos, PinKind::Input) {
            let gp = pin_grid_pos(
                &self.components[pck].spec.shape(),
                self.components[pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some(ptk) = tunnel_pin_at_pos(self.active_tunnels(), pan, pos) {
            return (
                NodeAttach::Tunnel(ptk),
                tunnel_pin_grid(&self.tunnels[ptk]),
                true,
            );
        }
        if let Some(nk) = self.wiring.node_at_pos(pos, pan) {
            return (NodeAttach::Free, self.wiring.nodes[nk].pos, true);
        }
        if let Some((_, gp)) = self.wiring.segment_at_pos(pos, pan) {
            return (NodeAttach::Free, gp, true);
        }
        (NodeAttach::Free, snap_to_grid(pos, pan), false)
    }

    // A wire may only start on a real terminal (pin, tunnel, or existing wire),
    // not in empty space.
    fn wire_start_at(&self, pos: Pos2, pan: Vec2) -> Option<(NodeAttach, GridPos)> {
        let (attach, gp, terminal) = self.wire_target_at(pos, pan);
        terminal.then_some((attach, gp))
    }

    // ── Save / load ──────────────────────────────────────────────────────

    pub fn to_circuit_file(&self) -> CircuitFile {
        // PlacedCompKey/PlacedTunnelKey -> position in the emitted Vec, so
        // wire nodes reference components/tunnels by index instead of a
        // slotmap key. Only live records are persisted - tombstones never
        // reach the save file.
        let mut comp_index: HashMap<PlacedCompKey, usize> = HashMap::new();
        let components: Vec<ComponentEntry> = self
            .active_components()
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
        let tunnels: Vec<TunnelEntry> = self
            .active_tunnels()
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
        // index. Built before `segments` reads it.
        let mut node_index: HashMap<crate::gui::wiring::WireNodeKey, usize> = HashMap::new();
        // Only live nodes/segments are persisted - tombstones (kept for undo)
        // never reach the save file. Active segments only reference active
        // nodes, so every SegEntry lookup below resolves.
        let nodes: Vec<NodeEntry> = self
            .wiring
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

        let segments = self
            .wiring
            .active_segments()
            .map(|(_, s)| SegEntry {
                a: node_index[&s.a],
                b: node_index[&s.b],
            })
            .collect();

        CircuitFile::new(components, tunnels, nodes, segments)
    }

    // Replaces the current circuit with the one described by `file`.
    // Validates first so a malformed file is rejected before any existing
    // state is touched.
    pub fn load_circuit_file(&mut self, file: &CircuitFile) -> Result<(), LoadError> {
        file.validate()?;

        self.circuit = Circuit::new();
        self.components = SlotMap::default();
        self.tunnels = SlotMap::default();
        self.wiring = Wiring::new();
        self.selected = None;
        self.mode = InteractionMode::Idle;
        self.last_settle_error = None;

        // File indices -> the freshly placed GUI keys (wiring nodes reference
        // components/tunnels by these).
        let comp_keys: Vec<PlacedCompKey> = file
            .components
            .iter()
            .map(|entry| self.place_component(entry.spec.clone(), entry.grid_pos))
            .collect();

        let tunnel_keys: Vec<PlacedTunnelKey> = file
            .tunnels
            .iter()
            .map(|entry| self.place_tunnel_labeled(entry.label.clone(), entry.role, entry.grid_pos))
            .collect();

        let node_keys: Vec<crate::gui::wiring::WireNodeKey> = file
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
                self.wiring.nodes.insert(WireNode {
                    pos: entry.pos,
                    attach,
                    active: true,
                })
            })
            .collect();

        for s in &file.segments {
            self.wiring.segments.insert(WireSegment {
                a: node_keys[s.a],
                b: node_keys[s.b],
                active: true,
            });
        }

        self.rebuild_circuit();
        Ok(())
    }

    /// Shows the properties panel for the selected item. Edits call
    /// `self.reconfigure_component()` with an updated `ComponentSpec`.
    fn show_properties(&mut self, ui: &mut egui::Ui) {
        let sel = match &self.selected {
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
        // The whole editor is read-only during a run session. Future
        // runtime-drivable components (e.g. a Button, or live Input toggles)
        // would carve their specific widgets out of this lock - e.g. via a
        // `spec.accepts_runtime_input()` predicate consulted inside the
        // per-component editors - rather than being blanket-disabled here.
        let locked = self.editing_locked();
        ui.add_enabled_ui(!locked, |ui| {
            match sel {
                Selected::Component(key) => self.show_component_properties(key, ui),
                Selected::Tunnel(key) => self.show_tunnel_properties(key, ui),
                Selected::Wire(_) => {
                    ui.heading("WIRE");
                    ui.label("A wire segment. Press Backspace or Delete to remove it.");
                }
            }

            ui.separator();
            if ui.button("Delete").clicked() {
                match sel {
                    Selected::Component(key) => self.delete_component(key),
                    Selected::Tunnel(key) => self.delete_tunnel(key),
                    Selected::Wire(seg) => self.delete_wire(seg),
                }
            }
        });
    }

    fn show_tunnel_properties(&mut self, key: PlacedTunnelKey, ui: &mut egui::Ui) {
        let role = self.tunnels[key].role;
        let tunnel_key = self.tunnels[key].key;

        ui.heading(match role {
            TunnelRole::Feed => "TUNNEL (FEED)",
            TunnelRole::Pull => "TUNNEL (PULL)",
        });
        ui.separator();
        ui.label("Label:");
        let mut label = self.tunnels[key].label.clone();
        let response = ui.text_edit_singleline(&mut label);
        if response.changed() {
            self.tunnels[key].label = label.clone();
        }

        // Commit on any focus loss (Enter/Tab/click-away), not just Enter: the
        // record label is already updated live above (on `changed()`), but the
        // circuit's hasn't committed yet, so read the old label from the
        // circuit to both detect a real change and capture undo's restore
        // value. (rebuild_circuit also reconciles as a backstop.)
        if response.lost_focus() {
            let old_label = self
                .circuit
                .tunnels
                .get(tunnel_key)
                .map(|t| t.label.clone());
            if old_label.as_deref() != Some(label.as_str()) {
                self.history.begin_batch();
                self.apply(Command::RenameTunnel {
                    tunnel: tunnel_key,
                    new_label: label.clone(),
                });
                // Record the record-side label change's undo (the Sim
                // RenameTunnel above only reverses the circuit's copy).
                if let Some(old) = old_label {
                    self.history
                        .push_gui(GuiUndoAction::SetTunnelLabel { key, label: old });
                }
                self.tunnels[key].label = label;
                self.history.end_batch();
                let result = self.circuit.settle();
                self.record_settle_result(result);
            }
        }
    }

    fn show_component_properties(&mut self, key: PlacedCompKey, ui: &mut egui::Ui) {
        let pc = &self.components[key];
        let comp_key = pc.key;

        ui.heading(pc.spec.label());
        ui.separator();

        let spec = pc.spec.clone();
        match spec {
            ComponentSpec::Input(Input {
                mut bits,
                mut width,
            }) => {
                let mut changed = false;
                ui.label(format!("Value: 0x{:X}", bits));

                // `bits` controlled by checkbox or textfield depending on `width`
                if width == 1 {
                    let mut high = bits != 0;
                    if ui.checkbox(&mut high, "Toggle").clicked() {
                        bits = high as u32;
                        changed = true;
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.label("Bits:");
                        changed |= ui
                            .add(egui::DragValue::new(&mut bits).range(0..=Value::mask(width)))
                            .changed();
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut width).range(1..=32))
                        .changed();
                });
                if changed {
                    bits &= Value::mask(width); // In case width was changed below max `bits` value
                    self.reconfigure_component(key, ComponentSpec::Input(Input { bits, width }));
                }
            }
            ComponentSpec::Output => {
                let val = self.circuit.read_output(comp_key);
                let val_str = match val {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                    Value::Invalid => "Invalid (width mismatch)".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
            ComponentSpec::Gate(Gate {
                op,
                mut n_inputs,
                mut width,
            }) => {
                let mut changed = false;
                if op != GateOp::Not {
                    ui.horizontal(|ui| {
                        ui.label("Inputs:");
                        changed |= ui
                            .add(egui::DragValue::new(&mut n_inputs).range(2..=8))
                            .changed();
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Gate(Gate {
                            op,
                            n_inputs,
                            width,
                        }),
                    );
                }
            }
            ComponentSpec::Mux(Mux {
                mut data_width,
                mut sel_width,
            }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Sel width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut sel_width).range(1..=4))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Mux(Mux {
                            data_width,
                            sel_width,
                        }),
                    );
                }
            }
            ComponentSpec::Demux(Demux {
                mut data_width,
                mut sel_width,
            }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Sel width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut sel_width).range(1..=4))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Demux(Demux {
                            data_width,
                            sel_width,
                        }),
                    );
                }
            }
            ComponentSpec::Reg(RegConf { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentSpec::Reg(RegConf { data_width }));
                }

                let cur = self.circuit.components[comp_key].pins.out_cache[0];
                let val_str = match cur {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                    Value::Invalid => "Invalid (width mismatch)".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
            ComponentSpec::Counter(CounterConf {
                mut data_width,
                mut max_value,
                mut overflow_action,
            }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Max value:");
                    changed |= ui.add(egui::DragValue::new(&mut max_value)).changed();
                });
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
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Counter(CounterConf {
                            data_width,
                            max_value,
                            overflow_action,
                        }),
                    );
                }

                let q = self.circuit.components[comp_key].pins.out_cache[0];
                let carry = self.circuit.components[comp_key].pins.out_cache[1];
                let val_str = |v: Value| match v {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                    Value::Invalid => "Invalid (width mismatch)".to_string(),
                };
                ui.label(format!("Q: {}", val_str(q)));
                ui.label(format!("Carry: {}", val_str(carry)));
            }
            ComponentSpec::DFlipFlop(_)
            | ComponentSpec::TFlipFlop(_)
            | ComponentSpec::JKFlipFlop(_)
            | ComponentSpec::SRFlipFlop(_) => {
                let cur = self.circuit.components[comp_key].pins.out_cache[0];
                let val_str = match cur {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                    Value::Invalid => "Invalid (width mismatch)".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
            ComponentSpec::Encoder(Encoder { mut sel_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Sel width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut sel_width).range(0..=4))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentSpec::Encoder(Encoder { sel_width }));
                }
            }
            ComponentSpec::Adder(Adder { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentSpec::Adder(Adder { data_width }));
                }
            }
            ComponentSpec::Subtractor(Subtractor { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Subtractor(Subtractor { data_width }),
                    );
                }
            }
            ComponentSpec::Multiplier(Multiplier { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Multiplier(Multiplier { data_width }),
                    );
                }
            }
            ComponentSpec::Divider(Divider { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentSpec::Divider(Divider { data_width }));
                }
            }
            ComponentSpec::Comparator(Comparator { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Comparator(Comparator { data_width }),
                    );
                }
            }
            ComponentSpec::Splitter {
                mut width,
                mut arm_bits,
                mut direction,
            } => {
                let mut changed = false;

                let before_dir = direction;
                ui.horizontal(|ui| {
                    ui.label("Fan Direction:");
                    ui.selectable_value(&mut direction, FanDirection::Right, "Split");
                    ui.selectable_value(&mut direction, FanDirection::Left, "Combine");
                });
                changed |= direction != before_dir;

                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut width).range(1..=32))
                        .changed();
                });
                let mut arms = arm_bits.len() as u8;
                ui.horizontal(|ui| {
                    ui.label("Arms:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut arms).range(1..=16))
                        .changed();
                });

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

                if changed {
                    self.reconfigure_component(
                        key,
                        ComponentSpec::Splitter {
                            width,
                            arm_bits,
                            direction,
                        },
                    );
                }
            }
        }
    }

    // Swaps a placed component's parameters. PlacedCompKey stays stable, so
    // attached wires survive - we only drop wire nodes for pins the new
    // arity no longer has, re-sync the rest, then rebuild.
    fn reconfigure_component(&mut self, pc_key: PlacedCompKey, new_spec: ComponentSpec) {
        self.history.begin_batch();
        let old_key = self.components[pc_key].key;
        let grid_pos = self.components[pc_key].grid_pos;

        let new_comp = new_spec.to_component();
        let new_n_in = new_comp.pins.inputs.len();
        let new_n_out = new_comp.pins.outputs.len();

        self.apply(Command::RemoveComponent(old_key));
        let new_key = self.apply(Command::AddComponent(new_comp)).unwrap_comp();
        // Record the record swap's undo before overwriting: restores the old
        // CompKey + def (the Sim actions above reactivate the old circuit comp
        // and deactivate the new one, but the record itself needs restoring).
        let old_spec = std::mem::replace(
            &mut self.components[pc_key],
            PlacedComponent {
                key: new_key,
                spec: new_spec,
                grid_pos,
                active: true,
            },
        )
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

    // Removes a placed component: drop it from the circuit and its wire nodes
    // from the wiring graph, then rebuild the circuit's nets from what remains.
    fn delete_component(&mut self, key: PlacedCompKey) {
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
    fn delete_tunnel(&mut self, key: PlacedTunnelKey) {
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
    fn delete_wire(&mut self, seg: WireSegKey) {
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
    fn is_highlighted(&self, sel: Selected) -> bool {
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
    fn drag_grid_pos(&self, sel: Selected, pan: Vec2) -> Option<(Rect, GridPos)> {
        match sel {
            Selected::Component(key) => self
                .components
                .get(key)
                .map(|pc| (component_bounding_rect(pc, pan), pc.grid_pos)),
            Selected::Tunnel(key) => self
                .tunnels
                .get(key)
                .map(|pt| (tunnel_bounding_rect(pt, pan), pt.grid_pos)),
            Selected::Wire(_) => None,
        }
    }

    // The Free-attached WireNodes belonging to any Selected::Wire in `sels`
    // (deduped - a route's interior node can be shared by two selected
    // segments), each paired with its current grid_pos. Pin/Tunnel-attached
    // nodes are excluded: those follow their owning component/tunnel via
    // sync_component_wire_nodes/sync_tunnel_wire_nodes instead, and must
    // stay put if that owner isn't itself part of the drag.
    fn free_wire_nodes(&self, sels: &[Selected]) -> Vec<(WireNodeKey, GridPos)> {
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
    fn items_in_rect(&self, rect: Rect, pan: Vec2) -> Vec<Selected> {
        puffin::profile_function!();
        let mut out = Vec::new();
        for (key, pc) in self.active_components() {
            if rect.contains_rect(component_bounding_rect(pc, pan)) {
                out.push(Selected::Component(key));
            }
        }
        for (key, pt) in self.active_tunnels() {
            if rect.contains_rect(tunnel_bounding_rect(pt, pan)) {
                out.push(Selected::Tunnel(key));
            }
        }
        for (key, seg) in self.wiring.active_segments() {
            let a = grid_to_screen(self.wiring.nodes[seg.a].pos, pan);
            let b = grid_to_screen(self.wiring.nodes[seg.b].pos, pan);
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
    fn delete_bulk(&mut self) {
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

    // Snapshots the current selection onto the clipboard. No-op if nothing
    // is selected. Read-only: never touches history.
    fn copy_selection(&mut self) {
        let items: Vec<Selected> = match &self.selected {
            None => return,
            Some(Selection::Single(s)) => vec![*s],
            Some(Selection::Bulk(v)) => v.clone(),
        };
        self.clipboard
            .copy(&self.components, &self.tunnels, &self.wiring, &items);
    }

    // Materializes the clipboard's (offset) snapshot as new components,
    // tunnels, and wiring, as one undoable batch; the pasted items become
    // the new selection. No-op if the clipboard is empty.
    fn paste_clipboard(&mut self) {
        let Some(file) = self.clipboard.plan_paste() else {
            return;
        };
        self.history.begin_batch();

        // File indices -> the freshly placed GUI keys, mirroring
        // load_circuit_file (wiring nodes reference components/tunnels by
        // these).
        let comp_keys: Vec<PlacedCompKey> = file
            .components
            .iter()
            .map(|entry| self.place_component(entry.spec.clone(), entry.grid_pos))
            .collect();

        let tunnel_keys: Vec<PlacedTunnelKey> = file
            .tunnels
            .iter()
            .map(|entry| self.place_tunnel_labeled(entry.label.clone(), entry.role, entry.grid_pos))
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

        let (_, seg_keys, delta) = self.wiring.add_subgraph(&nodes, &segments);
        self.edit_wiring(delta);
        self.rebuild_circuit();

        let mut new_selection: Vec<Selected> = Vec::new();
        new_selection.extend(comp_keys.into_iter().map(Selected::Component));
        new_selection.extend(tunnel_keys.into_iter().map(Selected::Tunnel));
        new_selection.extend(seg_keys.into_iter().map(Selected::Wire));
        self.selected = match new_selection.len() {
            0 => None,
            1 => Some(Selection::Single(new_selection[0])),
            _ => Some(Selection::Bulk(new_selection)),
        };

        self.history.end_batch();
    }

    // Applies a File > Load result that a spawned WASM load task has
    // delivered into `pending_load`, if any is waiting. No-op most frames.
    #[cfg(target_arch = "wasm32")]
    fn apply_pending_load(&mut self) {
        let Some(outcome) = self.pending_load.borrow_mut().take() else {
            return;
        };
        match outcome {
            Ok(file) => {
                if let Err(e) = self.load_circuit_file(&file) {
                    self.last_settle_error = Some(format!("load failed: {e}"));
                }
            }
            Err(e) => self.last_settle_error = Some(format!("load failed: {e}")),
        }
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
                        match self.to_circuit_file().to_json() {
                            Ok(json) => {
                                #[cfg(not(target_arch = "wasm32"))]
                                if let Some(Err(e)) = crate::io::native::save_dialog(&json) {
                                    self.last_settle_error = Some(format!("save failed: {e}"));
                                }
                                #[cfg(target_arch = "wasm32")]
                                crate::io::wasm::trigger_download(&json);
                            }
                            Err(e) => self.last_settle_error = Some(format!("save failed: {e}")),
                        }
                        ui.close();
                    }
                    if ui.add_enabled(!locked, egui::Button::new("Load")).clicked() {
                        #[cfg(not(target_arch = "wasm32"))]
                        if let Some(outcome) = crate::io::native::load_dialog() {
                            match outcome {
                                Ok(file) => {
                                    if let Err(e) = self.load_circuit_file(&file) {
                                        self.last_settle_error = Some(format!("load failed: {e}"));
                                    }
                                }
                                Err(e) => {
                                    self.last_settle_error = Some(format!("load failed: {e}"))
                                }
                            }
                        }
                        // WASM's file pick + read is async - this just kicks the
                        // task off; the result lands in pending_load and is
                        // applied by apply_pending_load on a later frame.
                        #[cfg(target_arch = "wasm32")]
                        crate::io::wasm::spawn_load_dialog(self.pending_load.clone());
                        ui.close();
                    }
                });
                ui.add_enabled_ui(!locked, |ui| {
                    ui.menu_button("Edit", |ui| {
                        if ui
                            .add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                            .clicked()
                        {
                            self.undo();
                            ui.close();
                        }
                        if ui
                            .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                            .clicked()
                        {
                            self.redo();
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(self.selected.is_some(), egui::Button::new("Copy"))
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
                ui.add_enabled_ui(!locked, |ui| {
                    ui.menu_button("Add", |ui| {
                        ui.menu_button("Gates", |ui| {
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
                                    self.mode = InteractionMode::Placing {
                                        spec: ComponentSpec::Gate(Gate {
                                            op,
                                            n_inputs: n,
                                            width: 1,
                                        }),
                                    };
                                    ui.close();
                                }
                            }
                        });
                        if ui.button("Input").clicked() {
                            self.mode = InteractionMode::Placing {
                                spec: ComponentSpec::Input(Input { bits: 0, width: 1 }),
                            };
                            ui.close();
                        }
                        if ui.button("Output").clicked() {
                            self.mode = InteractionMode::Placing {
                                spec: ComponentSpec::Output,
                            };
                            ui.close();
                        }

                        ui.menu_button("Plexers", |ui| {
                            if ui.button("Mux").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Mux(Mux {
                                        data_width: 1,
                                        sel_width: 1,
                                    }),
                                };
                                ui.close();
                            }
                            if ui.button("Demux").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Demux(Demux {
                                        data_width: 1,
                                        sel_width: 1,
                                    }),
                                };
                                ui.close();
                            }
                            if ui.button("Splitter").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Splitter {
                                        width: 2,
                                        arm_bits: vec![vec![0], vec![1]],
                                        direction: FanDirection::Right,
                                    },
                                };
                                ui.close();
                            }
                            if ui.button("Encoder").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Encoder(Encoder { sel_width: 1 }),
                                };
                                ui.close();
                            }
                        });
                        ui.menu_button("Arithmetic", |ui| {
                            if ui.button("Adder").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Adder(Adder { data_width: 1 }),
                                };
                                ui.close();
                            }
                            if ui.button("Subtractor").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Subtractor(Subtractor { data_width: 1 }),
                                };
                                ui.close();
                            }
                            if ui.button("Multiplier").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Multiplier(Multiplier { data_width: 1 }),
                                };
                                ui.close();
                            }
                            if ui.button("Divider").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Divider(Divider { data_width: 1 }),
                                };
                                ui.close();
                            }
                            if ui.button("Comparator").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Comparator(Comparator { data_width: 1 }),
                                };
                                ui.close();
                            }
                        });
                        ui.menu_button("Memory", |ui| {
                            if ui.button("Register").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Reg(RegConf { data_width: 1 }),
                                };
                                ui.close();
                            }
                            ui.menu_button("Flip-Flop", |ui| {
                                if ui.button("D Flip-Flop").clicked() {
                                    self.mode = InteractionMode::Placing {
                                        spec: ComponentSpec::DFlipFlop(DFlipFlopConf),
                                    };
                                    ui.close();
                                }
                                if ui.button("T Flip-Flop").clicked() {
                                    self.mode = InteractionMode::Placing {
                                        spec: ComponentSpec::TFlipFlop(TFlipFlopConf),
                                    };
                                    ui.close();
                                }
                                if ui.button("JK Flip-Flop").clicked() {
                                    self.mode = InteractionMode::Placing {
                                        spec: ComponentSpec::JKFlipFlop(JKFlipFlopConf),
                                    };
                                    ui.close();
                                }
                                if ui.button("SR Flip-Flop").clicked() {
                                    self.mode = InteractionMode::Placing {
                                        spec: ComponentSpec::SRFlipFlop(SRFlipFlopConf),
                                    };
                                    ui.close();
                                }
                            });
                            if ui.button("Counter").clicked() {
                                self.mode = InteractionMode::Placing {
                                    spec: ComponentSpec::Counter(CounterConf {
                                        data_width: 1,
                                        max_value: 1,
                                        overflow_action: OverflowAction::default(),
                                    }),
                                };
                                ui.close();
                            }
                        });
                        ui.menu_button("Tunnel", |ui| {
                            if ui.button("Feed").clicked() {
                                self.mode = InteractionMode::PlacingTunnel {
                                    role: TunnelRole::Feed,
                                };
                                ui.close();
                            }
                            if ui.button("Pull").clicked() {
                                self.mode = InteractionMode::PlacingTunnel {
                                    role: TunnelRole::Pull,
                                };
                                ui.close();
                            }
                        });
                    });
                });
                ui.menu_button("Debug", |ui| {
                    ui.checkbox(&mut self.show_profiler, "Profiler");
                });
                ui.separator();
                self.show_clock_controls(ui);
                if let Some(err) = &self.last_settle_error {
                    ui.colored_label(theme.error_text, err);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("v{APP_VERSION} ({GIT_SHA})"));
                });
            })
        });
    }

    // The clock transport: a speed setting plus Play / Pause / Step / Stop.
    // Buttons are enable-gated on the current run state (see the state table in
    // ClockRun); entering Play locks editing for the whole session and Stop
    // resets sequential state. All ticks are issued untracked (see tick_once).
    fn show_clock_controls(&mut self, ui: &mut egui::Ui) {
        let run = self.clock.run;

        // Speed is only adjustable while stopped - locked during a run session.
        ui.add_enabled(
            run == ClockRun::Stopped,
            egui::DragValue::new(&mut self.clock.ticks_per_second)
                .speed(0.1)
                .range(1.0..=60.0)
                .suffix(" tick/s"),
        );

        // Play: start (from Stopped) or resume (from Paused) auto-advancing.
        // Resets the auto-advance clock and abandons any in-progress placement
        // so nothing can edit mid-run.
        if ui
            .add_enabled(run != ClockRun::Playing, egui::Button::new("Play"))
            .clicked()
        {
            self.clock.run = ClockRun::Playing;
            self.clock.last_tick_time = ui.ctx().input(|i| i.time);
            self.mode = InteractionMode::Idle;
        }

        // Pause: freeze mid-run, preserving sequential state (stays locked).
        if ui
            .add_enabled(run == ClockRun::Playing, egui::Button::new("Pause"))
            .clicked()
        {
            self.clock.run = ClockRun::Paused;
        }

        // Step: advance exactly one tick. Available when not playing - from
        // Stopped it's a single manual tick (stays editable); from Paused it
        // nudges the frozen run forward one step.
        if ui
            .add_enabled(run != ClockRun::Playing, egui::Button::new("Step"))
            .clicked()
        {
            self.tick_once();
        }

        // Stop: halt, reset all sequential state to power-on, return to editable.
        if ui
            .add_enabled(run != ClockRun::Stopped, egui::Button::new("Stop"))
            .clicked()
        {
            self.stop_clock();
        }
    }

    // ── Canvas drawing ────────────────────────────────────────────────────
    fn draw_canvas(&self, painter: &Painter, clip_rect: Rect, pan: Vec2, theme: Theme) {
        puffin::profile_function!();
        painter.rect_filled(clip_rect, 0.0, theme.canvas_bg);
        draw_grid(painter, clip_rect, pan, theme);

        // Draw wire segments. Colour comes from each connected group's net
        // value: any component pin / tunnel on the group resolves (live) to
        // that net's Value; a dangling group (no endpoints) is Floating.
        let node_value = self.wire_node_values();

        for (seg_key, seg) in self.wiring.active_segments() {
            let a = self.wiring.nodes[seg.a];
            let b = self.wiring.nodes[seg.b];
            let p0 = grid_to_screen(a.pos, pan);
            let p1 = grid_to_screen(b.pos, pan);
            let val = node_value.get(&seg.a).copied().unwrap_or(Value::Floating);
            let mut stroke = value_stroke(theme, val);
            if self.is_highlighted(Selected::Wire(seg_key)) {
                stroke.color = theme.outline_selected;
                stroke.width += 1.5;
            }
            painter.line_segment([p0, p1], stroke);
        }

        // Junction dots where three or more segments meet, so a real branch
        // reads differently from a mere crossing.
        for (nk, node) in self.wiring.active_nodes() {
            if self.wiring.degree(nk) >= 3 {
                let val = node_value.get(&nk).copied().unwrap_or(Value::Floating);
                painter.circle_filled(
                    grid_to_screen(node.pos, pan),
                    PIN_RADIUS,
                    value_stroke(theme, val).color,
                );
            }
        }

        // Draw components

        for (pc_key, pc) in self.active_components() {
            let is_selected = self.is_highlighted(Selected::Component(pc_key));
            draw_component(painter, pc, pan, &self.circuit, is_selected, theme);
        }

        // Draw tunnels

        for (pt_key, pt) in self.active_tunnels() {
            let is_selected = self.is_highlighted(Selected::Tunnel(pt_key));
            draw_tunnel(painter, pt, pan, &self.circuit, is_selected, theme);
        }
    }

    // ── Global canvas keyboard shortcuts ─────────────────────────────────
    // Escape (cancel drag/wire-draw, clear selection), Delete/Backspace, and
    // Undo/Redo. Must run before `handle_canvas_interaction` reads `self.mode`
    // in the same frame, since Escape can reset it to Idle.
    fn handle_canvas_shortcuts(&mut self, ctx: &egui::Context) {
        puffin::profile_function!();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            match &self.mode {
                InteractionMode::SelectionDrag {
                    items, free_nodes, ..
                } => {
                    for (key, original_grid_pos) in items {
                        match key {
                            Selected::Component(k) => {
                                self.components[*k].grid_pos = *original_grid_pos
                            }
                            Selected::Tunnel(k) => self.tunnels[*k].grid_pos = *original_grid_pos,
                            Selected::Wire(_) => {}
                        }
                    }
                    for (key, original_pos) in free_nodes {
                        self.wiring.nodes[*key].pos = *original_pos;
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
                    self.commit_wire_route(points, start_attach, NodeAttach::Free);
                }
                // BulkSelect: Esc cancels the in-progress rubber-band (the
                // trailing reset to Idle handles it) alongside clearing any
                // existing bulk selection below.
                _ => {}
            }
            if matches!(self.selected, Some(Selection::Bulk(_))) {
                self.selected = None;
            }
            self.mode = InteractionMode::Idle;
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
            match &self.selected {
                Some(Selection::Bulk(_)) => self.delete_bulk(),
                Some(Selection::Single(sel)) => match *sel {
                    Selected::Component(k) => self.delete_component(k),
                    Selected::Tunnel(k) => self.delete_tunnel(k),
                    Selected::Wire(seg) => self.delete_wire(seg),
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
                self.undo();
            } else if redo {
                self.redo();
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
        let mode = self.mode.clone();
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
        // Hover reticle: hovering over a wire (but not a pin) shows
        // where a branch would tap the wire.
        if let Some(pos) = pointer {
            if pin_at_pos(self.active_components(), cc.pan, pos, PinKind::Output).is_none()
                && pin_at_pos(self.active_components(), cc.pan, pos, PinKind::Input).is_none()
                && tunnel_pin_at_pos(self.active_tunnels(), cc.pan, pos).is_none()
            {
                if let Some((_, gp)) = self.wiring.segment_at_pos(pos, cc.pan) {
                    draw_reticle(cc.painter, grid_to_screen(gp, cc.pan), cc.theme);
                }
            }
        }

        // All drag gestures (wire draw, component/bulk move, rubber-band
        // select) mutate the circuit or selection - suppressed during a run
        // session. Plain click-to-select below stays available for inspection.
        if !self.editing_locked() && cc.response.drag_started() {
            let origin = cc.ctx.input(|i| i.pointer.press_origin());
            if let Some(pos) = origin {
                if let Some((attach, gp)) = self.wire_start_at(pos, cc.pan) {
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
                        self.drag_grid_pos(sel, cc.pan)
                            .filter(|(rect, _)| rect.contains(pos))
                            .map(|(_, grid_pos)| (vec![(sel, grid_pos)], Vec::new()))
                    }
                    Some(Selection::Bulk(sels)) => {
                        // Bulk body drag → move every selected component/
                        // tunnel together, plus any Free wire node the
                        // selection also covers, as long as the drag began
                        // inside *any one* component/tunnel's bounding rect.
                        let started_inside = sels.iter().any(|sel| {
                            self.drag_grid_pos(*sel, cc.pan)
                                .is_some_and(|(rect, _)| rect.contains(pos))
                        });
                        started_inside.then(|| {
                            let items: Vec<(Selected, GridPos)> = sels
                                .iter()
                                .filter_map(|sel| {
                                    self.drag_grid_pos(*sel, cc.pan).map(|(_, gp)| (*sel, gp))
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
                    let gp = snap_to_grid(pos, cc.pan);
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
                    .wire_start_at(pos, cc.pan)
                    .filter(|(a, _)| matches!(a, NodeAttach::Pin(..) | NodeAttach::Tunnel(_)))
                    .filter(|_| !self.editing_locked());
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
                        .components
                        .iter()
                        .find(|(_k, pc)| component_bounding_rect(pc, cc.pan).contains(pos))
                        .map(|(k, _)| Selected::Component(k));
                    let maybe_tunnel = self
                        .tunnels
                        .iter()
                        .find(|(_k, pt)| tunnel_bounding_rect(pt, cc.pan).contains(pos))
                        .map(|(k, _)| Selected::Tunnel(k));
                    let maybe_wire = self
                        .wiring
                        .segment_at_pos(pos, cc.pan)
                        .map(|(seg, _)| Selected::Wire(seg));
                    self.selected = maybe_comp
                        .or(maybe_tunnel)
                        .or(maybe_wire)
                        .map(Selection::Single);
                }
            }
        }
    }

    fn interact_placing(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>, spec: ComponentSpec) {
        if let Some(pos) = pointer {
            let gp = snap_to_grid(pos, cc.pan);
            draw_ghost(cc.painter, &spec, gp, cc.pan, cc.theme);
        }
        if cc.response.clicked() {
            if let Some(pos) = pointer {
                let gp = snap_to_grid(pos, cc.pan);
                self.place_component(spec, gp);
                self.mode = InteractionMode::Idle;
            }
        }
    }

    fn interact_placing_tunnel(&mut self, cc: &CanvasCtx, pointer: Option<Pos2>, role: TunnelRole) {
        if let Some(pos) = pointer {
            let gp = snap_to_grid(pos, cc.pan);
            draw_tunnel_ghost(cc.painter, role, gp, cc.pan, cc.theme);
        }
        if cc.response.clicked() {
            if let Some(pos) = pointer {
                let gp = snap_to_grid(pos, cc.pan);
                self.place_tunnel(role, gp);
                self.mode = InteractionMode::Idle;
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
        let (drop_attach, drop_gp, terminal) = self.wire_target_at(end, cc.pan);

        // Preview: committed segments, then the pending elbow from the
        // last committed corner to the (snapped) drop point.
        let preview = Stroke::new(WIRE_THICKNESS_THIN, cc.theme.wire_drag_preview);
        for w in points.windows(2) {
            cc.painter.line_segment(
                [grid_to_screen(w[0], cc.pan), grid_to_screen(w[1], cc.pan)],
                preview,
            );
        }
        let pending = route_elbow(*points.last().unwrap(), drop_gp);
        let mut prev = *points.last().unwrap();
        for p in &pending {
            cc.painter.line_segment(
                [grid_to_screen(prev, cc.pan), grid_to_screen(*p, cc.pan)],
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

    fn interact_component_drag(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        items: Vec<(Selected, GridPos)>,
        free_nodes: Vec<(WireNodeKey, GridPos)>,
        drag_origin: Pos2,
    ) {
        puffin::profile_function!();
        if let Some(pos) = pointer {
            let delta_x = ((pos.x - drag_origin.x) / GRID_SIZE).round() as i32;
            let delta_y = ((pos.y - drag_origin.y) / GRID_SIZE).round() as i32;
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
                        self.components[k].grid_pos = new_grid_pos;
                        self.sync_component_wire_nodes(k);
                    }
                    Selected::Tunnel(k) => {
                        self.tunnels[k].grid_pos = new_grid_pos;
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
                self.wiring.nodes[key].pos =
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

    fn interact_bulk_select(
        &mut self,
        cc: &CanvasCtx,
        pointer: Option<Pos2>,
        start: GridPos,
        current: GridPos,
    ) {
        puffin::profile_function!();
        // Track the live corner, then paint the rubber-band box.
        let current = pointer.map(|p| snap_to_grid(p, cc.pan)).unwrap_or(current);
        let rect = selection_screen_rect(start, current, cc.pan);
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
            let selected_items = self.items_in_rect(rect, cc.pan);
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

impl eframe::App for OsmilogApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Frame boundary for every puffin scope recorded this frame (in both
        // logic() and ui() - eframe calls them once each, in that order).
        puffin::GlobalProfiler::lock().new_frame();
        puffin::profile_function!();

        #[cfg(target_arch = "wasm32")]
        self.apply_pending_load();

        self.advance_clock(ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            #[cfg(not(target_arch = "wasm32"))]
            std::process::exit(0);
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

        egui::Panel::left("properties")
            .min_size(200.0)
            .resizable(true)
            .show(ui, |ui| {
                self.show_properties(ui);
            });

        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let clip_rect = painter.clip_rect();
        let pan = self.pan;

        self.handle_canvas_shortcuts(&ctx);
        self.draw_canvas(&painter, clip_rect, pan, theme);

        let cc = CanvasCtx {
            response: &response,
            painter: &painter,
            ctx: &ctx,
            pan,
            theme,
        };
        self.handle_canvas_interaction(&cc);
    }
}

// ── Geometry ─────────────────────────────────────────────────────────────────

fn component_bounding_rect(pc: &PlacedComponent, pan: Vec2) -> Rect {
    let size = pc.spec.size();
    let tl = egui::pos2(
        pc.grid_pos.x as f32 * GRID_SIZE + pan.x,
        pc.grid_pos.y as f32 * GRID_SIZE + pan.y,
    );
    Rect::from_min_size(tl, size)
}

// Grid coordinate of a pin: the component's grid_pos plus the anchor's whole-cell
// offset. This is the wiring counterpart of comp_pin_pos (which returns pixels).
fn pin_grid_pos(shape: &ComponentShape, grid_pos: GridPos, pin: PinId) -> GridPos {
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
fn tunnel_pin_grid(pt: &PlacedTunnel) -> GridPos {
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

fn grid_to_screen(gp: GridPos, pan: Vec2) -> Pos2 {
    egui::pos2(
        gp.x as f32 * GRID_SIZE + pan.x,
        gp.y as f32 * GRID_SIZE + pan.y,
    )
}

// The screen-space rectangle spanned by a BulkSelect drag's two grid corners,
// normalized so either drag direction yields the same box.
fn selection_screen_rect(start: GridPos, current: GridPos, pan: Vec2) -> Rect {
    Rect::from_two_pos(grid_to_screen(start, pan), grid_to_screen(current, pan))
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
fn comp_pin_pos(shape: &ComponentShape, grid_pos: GridPos, pan: Vec2, pin: PinId) -> Pos2 {
    let tl = egui::pos2(
        grid_pos.x as f32 * GRID_SIZE + pan.x,
        grid_pos.y as f32 * GRID_SIZE + pan.y,
    );
    let anchor = match pin {
        PinId::In(InIdx(i)) => &shape.input_anchors[i as usize],
        PinId::Out(OutIdx(i)) => &shape.output_anchors[i as usize],
    };
    // Anchors are whole grid cells from the top-left (itself grid-aligned), so
    // every pin lands exactly on a grid intersection.
    tl + anchor.cell * GRID_SIZE
}

fn tunnel_bounding_rect(pt: &PlacedTunnel, pan: Vec2) -> Rect {
    let size = tunnel_shape(pt.role).size;
    let tl = egui::pos2(
        pt.grid_pos.x as f32 * GRID_SIZE + pan.x,
        pt.grid_pos.y as f32 * GRID_SIZE + pan.y,
    );
    Rect::from_min_size(tl, size)
}

fn tunnel_pin_pos(pt: &PlacedTunnel, pan: Vec2) -> Pos2 {
    let shape = tunnel_shape(pt.role);
    let tl = egui::pos2(
        pt.grid_pos.x as f32 * GRID_SIZE + pan.x,
        pt.grid_pos.y as f32 * GRID_SIZE + pan.y,
    );
    let anchor = match pt.role {
        TunnelRole::Feed => &shape.output_anchors[0],
        TunnelRole::Pull => &shape.input_anchors[0],
    };
    tl + anchor.cell * GRID_SIZE
}

fn pin_at_pos<'a>(
    components: impl Iterator<Item = (PlacedCompKey, &'a PlacedComponent)>,
    pan: Vec2,
    pos: Pos2,
    kind: PinKind,
) -> Option<(PlacedCompKey, PinId)> {
    puffin::profile_function!();
    let hit_r = PIN_RADIUS * 2.0;
    for (key, pc) in components {
        let shape = pc.spec.shape();
        match kind {
            PinKind::Output => {
                for i in 0..pc.spec.n_outputs() {
                    let pp = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::output(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((key, PinId::output(i as u8)));
                    }
                }
            }
            PinKind::Input => {
                for i in 0..pc.spec.n_inputs() {
                    let pp = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::input(i as u8));
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
fn tunnel_pin_at_pos<'a>(
    tunnels: impl Iterator<Item = (PlacedTunnelKey, &'a PlacedTunnel)>,
    pan: Vec2,
    pos: Pos2,
) -> Option<PlacedTunnelKey> {
    puffin::profile_function!();
    let hit_r = PIN_RADIUS * 2.0;
    for (key, tunnel) in tunnels {
        if tunnel_pin_pos(tunnel, pan).distance(pos) <= hit_r {
            return Some(key);
        }
    }
    None
}

// ── Color ─────────────────────────────────────────────────────────────────────

fn value_stroke(theme: Theme, val: Value) -> Stroke {
    let (color, weight) = match val {
        Value::Floating => (theme.value_floating, WIRE_THICKNESS_THIN),
        Value::Invalid => (theme.value_invalid, WIRE_THICKNESS_THICK),
        Value::Fixed { bits, width } => (
            if bits == 0 {
                theme.value_low
            } else {
                theme.value_high
            },
            if width == 1 {
                WIRE_THICKNESS_THIN
            } else {
                WIRE_THICKNESS_THICK
            },
        ),
    };
    Stroke::new(weight, color)
}

// ── Drawing ───────────────────────────────────────────────────────────────────

fn draw_grid(painter: &Painter, clip_rect: Rect, pan: Vec2, theme: Theme) {
    let x0 = ((clip_rect.left() - pan.x) / GRID_SIZE).floor() as i32;
    let x1 = ((clip_rect.right() - pan.x) / GRID_SIZE).ceil() as i32;
    let y0 = ((clip_rect.top() - pan.y) / GRID_SIZE).floor() as i32;
    let y1 = ((clip_rect.bottom() - pan.y) / GRID_SIZE).ceil() as i32;
    for gx in x0..=x1 {
        for gy in y0..=y1 {
            painter.circle_filled(
                egui::pos2(gx as f32 * GRID_SIZE + pan.x, gy as f32 * GRID_SIZE + pan.y),
                1.0,
                theme.grid_dot,
            );
        }
    }
}

// A small crosshair marking where a branch would tap an existing wire.
fn draw_reticle(painter: &Painter, pos: Pos2, theme: Theme) {
    let r = PIN_RADIUS + 1.0;
    let stroke = Stroke::new(1.0, theme.wire_drag_preview);
    painter.line_segment([pos - egui::vec2(r, 0.0), pos + egui::vec2(r, 0.0)], stroke);
    painter.line_segment([pos - egui::vec2(0.0, r), pos + egui::vec2(0.0, r)], stroke);
}

fn draw_component(
    painter: &Painter,
    pc: &PlacedComponent,
    pan: Vec2,
    circuit: &Circuit,
    is_selected: bool,
    theme: Theme,
) {
    puffin::profile_function!();
    let shape = pc.spec.shape();
    let rect = component_bounding_rect(pc, pan);
    let fill = theme.component_fill;
    let (stroke_w, stroke_col) = if is_selected {
        (COMP_STROKE + 1.0, theme.outline_selected)
    } else {
        (COMP_STROKE, theme.outline_default)
    };
    let outline_stroke = Stroke::new(stroke_w, stroke_col);

    // Fill: use the convex fill_outline if provided (avoids epaint's concave polygon artifact),
    // otherwise fall back to the regular outline.
    let fill_pts = tessellate_path(
        shape.fill_outline.as_deref().unwrap_or(&shape.outline),
        rect,
    );
    painter.add(egui::Shape::Path(PathShape {
        points: fill_pts,
        closed: true,
        fill,
        stroke: Stroke::NONE.into(),
    }));

    // Stroke: always use the full outline (may include concave curves).
    let stroke_pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: stroke_pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(stroke_w, stroke_col),
    }));

    for stroke_cmds in &shape.extra_strokes {
        let stroke_pts = tessellate_path(stroke_cmds, rect);
        painter.add(egui::Shape::line(stroke_pts, outline_stroke));
    }

    for (i, &has_bubble) in shape.output_bubbles.iter().enumerate() {
        if has_bubble {
            let anchor = &shape.output_anchors[i];
            // The pin sits one cell beyond the body edge; the bubble is drawn in
            // the gap, just outside the edge (one cell back from the pin).
            let pin = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::output(i as u8));
            let edge = pin - anchor.wire_dir * GRID_SIZE;
            let center = edge + anchor.wire_dir * BUBBLE_R;
            painter.circle_filled(center, BUBBLE_R, fill);
            painter.circle_stroke(center, BUBBLE_R, outline_stroke);
        }
    }

    for label in &shape.labels {
        let label_pos = egui::pos2(
            rect.left() + label.pos.x * rect.width(),
            rect.top() + label.pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            label.text,
            FontId::monospace(label.font_size),
            theme.label_text,
        );
    }

    for i in 0..pc.spec.n_inputs() {
        let pos = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::input(i as u8));
        let val = circuit.components[pc.key].pins.inputs[i]
            .map(|nk| circuit.nets[nk].value)
            .unwrap_or(Value::Floating);
        painter.circle_filled(pos, PIN_RADIUS, value_stroke(theme, val).color);
    }
    for i in 0..pc.spec.n_outputs() {
        let pos = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::output(i as u8));
        let val = circuit.components[pc.key].pins.out_cache[i];
        painter.circle_filled(pos, PIN_RADIUS, value_stroke(theme, val).color);
    }
}

fn draw_tunnel(
    painter: &Painter,
    pt: &PlacedTunnel,
    pan: Vec2,
    circuit: &Circuit,
    is_selected: bool,
    theme: Theme,
) {
    puffin::profile_function!();
    let shape = tunnel_shape(pt.role);
    let rect = tunnel_bounding_rect(pt, pan);
    // Distinct fill from components (theme's "open" widget tone), to visually
    // distinguish tunnels.
    let fill = theme.tunnel_fill;
    let (stroke_w, stroke_col) = if is_selected {
        (COMP_STROKE + 1.0, theme.outline_selected)
    } else {
        (COMP_STROKE, theme.outline_default)
    };

    let fill_pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: fill_pts,
        closed: true,
        fill,
        stroke: Stroke::NONE.into(),
    }));

    let stroke_pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: stroke_pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(stroke_w, stroke_col),
    }));

    let label_pos = egui::pos2(
        rect.left() + shape.dynamic_label_pos.x * rect.width(),
        rect.top() + shape.dynamic_label_pos.y * rect.height(),
    );
    painter.text(
        label_pos,
        Align2::CENTER_CENTER,
        &pt.label,
        FontId::monospace(LABEL_FONT_SIZE),
        theme.label_text,
    );

    let val = circuit
        .tunnels
        .get(pt.key)
        .and_then(|t| t.net)
        .map(|nk| circuit.nets[nk].value)
        .unwrap_or(Value::Floating);
    painter.circle_filled(
        tunnel_pin_pos(pt, pan),
        PIN_RADIUS,
        value_stroke(theme, val).color,
    );
}

fn draw_ghost(painter: &Painter, spec: &ComponentSpec, grid_pos: GridPos, pan: Vec2, theme: Theme) {
    let shape = spec.shape();
    let tl = egui::pos2(
        grid_pos.x as f32 * GRID_SIZE + pan.x,
        grid_pos.y as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, shape.size);
    let ghost_col = theme.ghost_preview;

    let pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(COMP_STROKE, ghost_col),
    }));

    for stroke_cmds in &shape.extra_strokes {
        let stroke_pts = tessellate_path(stroke_cmds, rect);
        painter.add(egui::Shape::line(
            stroke_pts,
            Stroke::new(COMP_STROKE, ghost_col),
        ));
    }

    for label in &shape.labels {
        let label_pos = egui::pos2(
            rect.left() + label.pos.x * rect.width(),
            rect.top() + label.pos.y * rect.height(),
        );
        painter.text(
            label_pos,
            Align2::CENTER_CENTER,
            label.text,
            FontId::monospace(LABEL_FONT_SIZE),
            ghost_col,
        );
    }
}

fn draw_tunnel_ghost(
    painter: &Painter,
    role: TunnelRole,
    grid_pos: GridPos,
    pan: Vec2,
    theme: Theme,
) {
    let shape = tunnel_shape(role);
    let tl = egui::pos2(
        grid_pos.x as f32 * GRID_SIZE + pan.x,
        grid_pos.y as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, shape.size);
    let ghost_col = theme.ghost_preview;

    let pts = tessellate_path(&shape.outline, rect);
    painter.add(egui::Shape::Path(PathShape {
        points: pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(COMP_STROKE, ghost_col),
    }));

    let label_pos = egui::pos2(
        rect.left() + shape.dynamic_label_pos.x * rect.width(),
        rect.top() + shape.dynamic_label_pos.y * rect.height(),
    );
    let label = match role {
        TunnelRole::Feed => "TUN(F)",
        TunnelRole::Pull => "TUN(P)",
    };
    painter.text(
        label_pos,
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(LABEL_FONT_SIZE),
        ghost_col,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::wiring::NodeAttach;
    use crate::io::CURRENT_VERSION;
    use crate::sim::component::GateOp;

    fn place(app: &mut OsmilogApp, spec: ComponentSpec) -> PlacedCompKey {
        app.place_component(spec, GridPos::new(0, 0))
    }

    // Insert a wire (one segment) between two component pins, positioned at each
    // pin's grid cell, and return the two node keys.
    fn connect_pins(app: &mut OsmilogApp, a: (PlacedCompKey, PinId), b: (PlacedCompKey, PinId)) {
        let pa = pin_grid_pos(
            &app.components[a.0].spec.shape(),
            app.components[a.0].grid_pos,
            a.1,
        );
        let pb = pin_grid_pos(
            &app.components[b.0].spec.shape(),
            app.components[b.0].grid_pos,
            b.1,
        );
        let na = app.wiring.nodes.insert(WireNode {
            pos: pa,
            attach: NodeAttach::Pin(a.0, a.1),
            active: true,
        });
        let nb = app.wiring.nodes.insert(WireNode {
            pos: pb,
            attach: NodeAttach::Pin(b.0, b.1),
            active: true,
        });
        app.wiring.segments.insert(WireSegment {
            a: na,
            b: nb,
            active: true,
        });
    }

    // Insert a wire (one segment) between a component pin and a tunnel.
    fn connect_pin_tunnel(app: &mut OsmilogApp, c: (PlacedCompKey, PinId), ptk: PlacedTunnelKey) {
        let pc = pin_grid_pos(
            &app.components[c.0].spec.shape(),
            app.components[c.0].grid_pos,
            c.1,
        );
        let pt = tunnel_pin_grid(&app.tunnels[ptk]);
        let nc = app.wiring.nodes.insert(WireNode {
            pos: pc,
            attach: NodeAttach::Pin(c.0, c.1),
            active: true,
        });
        let nt = app.wiring.nodes.insert(WireNode {
            pos: pt,
            attach: NodeAttach::Tunnel(ptk),
            active: true,
        });
        app.wiring.segments.insert(WireSegment {
            a: nc,
            b: nt,
            active: true,
        });
    }

    #[test]
    fn test_circuit_file_round_trip_basic() {
        let mut app = OsmilogApp::empty();
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
        app.rebuild_circuit();

        let o_key = app.components[o].key;
        assert_eq!(app.circuit.read_output(o_key), Value::ONE);

        // Save -> JSON -> parse -> load into a fresh app, and confirm the
        // loaded circuit behaves identically.
        let file = app.to_circuit_file();
        let json = file.to_json().unwrap();
        let file2 = CircuitFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_circuit_file(&file2).unwrap();

        assert_eq!(loaded.components.len(), 4);
        assert_eq!(loaded.wiring.segments.len(), 3);
        assert_eq!(loaded.wiring.nodes.len(), 6);
        let loaded_out_key = loaded
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.circuit.read_output(loaded_out_key), Value::ONE);
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
        app.rebuild_circuit();

        // Delete the a->g wire; its nodes/segment become tombstones still held
        // in the SlotMaps.
        let seg = app
            .wiring
            .active_segments()
            .find(|(_, s)| {
                matches!(app.wiring.nodes[s.a].attach, NodeAttach::Pin(k, _) if k == a)
                    || matches!(app.wiring.nodes[s.b].attach, NodeAttach::Pin(k, _) if k == a)
            })
            .map(|(k, _)| k)
            .unwrap();
        app.delete_wire(seg);
        assert!(app.wiring.segments.len() > app.wiring.active_segments().count());

        // The file carries only live entries, and the reload matches the live
        // graph exactly.
        let file = app.to_circuit_file();
        assert_eq!(file.segments.len(), app.wiring.active_segments().count());
        assert_eq!(file.nodes.len(), app.wiring.active_nodes().count());

        let json = file.to_json().unwrap();
        let file2 = CircuitFile::from_json(&json).unwrap();
        let mut loaded = OsmilogApp::empty();
        loaded.load_circuit_file(&file2).unwrap();
        assert_eq!(
            loaded.wiring.active_segments().count(),
            app.wiring.active_segments().count()
        );
        // A fresh load has no tombstones: every stored entry is live.
        assert_eq!(
            loaded.wiring.segments.len(),
            loaded.wiring.active_segments().count()
        );
        assert_eq!(
            loaded.wiring.nodes.len(),
            loaded.wiring.active_nodes().count()
        );
    }

    #[test]
    fn test_circuit_file_round_trip_with_tunnel() {
        let mut app = OsmilogApp::empty();
        let inp = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let out = place(&mut app, ComponentSpec::Output);
        let feed = app.place_tunnel(TunnelRole::Feed, GridPos::new(0, 0));
        let pull = app.place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));

        // Tunnels connect to each other by matching label, not by wire -
        // give `pull` the same label as `feed` so they form one virtual net.
        let shared_label = app.tunnels[feed].label.clone();
        app.circuit
            .rename_tunnel(app.tunnels[pull].key, shared_label.clone());
        app.tunnels[pull].label = shared_label;

        // Pull reads FROM inp's output; Feed drives out's input.
        connect_pin_tunnel(&mut app, (inp, PinId::output(0)), pull);
        connect_pin_tunnel(&mut app, (out, PinId::input(0)), feed);
        app.rebuild_circuit();

        let out_key = app.components[out].key;
        assert_eq!(app.circuit.read_output(out_key), Value::ONE);

        let file = app.to_circuit_file();
        let json = file.to_json().unwrap();
        let file2 = CircuitFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_circuit_file(&file2).unwrap();

        assert_eq!(loaded.tunnels.len(), 2);
        let loaded_out_key = loaded
            .components
            .values()
            .find(|pc| matches!(pc.spec, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.circuit.read_output(loaded_out_key), Value::ONE);
    }

    #[test]
    fn test_load_circuit_file_rejects_bad_component_index() {
        let file = CircuitFile {
            version: CURRENT_VERSION,
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
        };

        let mut app = OsmilogApp::empty();
        let before = app.components.len();
        assert!(app.load_circuit_file(&file).is_err());
        // A rejected file must not leave the app half-overwritten.
        assert_eq!(app.components.len(), before);
    }

    #[test]
    fn test_load_circuit_file_rejects_unsupported_version() {
        let file = CircuitFile {
            version: CURRENT_VERSION + 1,
            components: vec![],
            tunnels: vec![],
            nodes: vec![],
            segments: vec![],
        };

        let mut app = OsmilogApp::empty();
        assert_eq!(
            app.load_circuit_file(&file),
            Err(LoadError::UnsupportedVersion {
                found: CURRENT_VERSION + 1,
                supported: CURRENT_VERSION,
            })
        );
    }

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
        app.rebuild_circuit();

        let g_key = app.components[g].key;
        let o_key = app.components[o].key;
        assert_eq!(app.circuit.read_output(o_key), Value::ZERO); // NOT(1) = 0
        app.selected = Some(Selection::Single(Selected::Component(g)));

        app.delete_component(g);

        // The placed record is tombstoned (kept for undo), so its key stays
        // valid but the record is inactive rather than gone.
        assert!(app.components.contains_key(g));
        assert!(!app.components[g].active);
        // Circuit-side removal tombstones (keeps the CompKey for undo), so the
        // component is inactive rather than gone.
        assert!(app.circuit.components.get(g_key).is_some_and(|c| !c.active));
        // No wire node references the deleted component; orphan neighbours were
        // cleaned up too, leaving no segments.
        assert!(app
            .wiring
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Pin(k, _) if k == g)));
        assert_eq!(app.wiring.active_segments().count(), 0);
        assert_eq!(app.selected, None);
        // Output's input pin is now Floating.
        assert_eq!(app.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_tunnel_drops_wire_nodes() {
        // A component pin wired to a tunnel: deleting the tunnel removes its wire
        // nodes and clears the selection.
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let t = app.place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let t_key = app.tunnels[t].key;
        connect_pin_tunnel(&mut app, (a, PinId::output(0)), t);
        app.rebuild_circuit();
        app.selected = Some(Selection::Single(Selected::Tunnel(t)));

        app.delete_tunnel(t);

        // Placed record tombstoned (kept for undo): key valid, record inactive.
        assert!(app.tunnels.contains_key(t));
        assert!(!app.tunnels[t].active);
        // Tombstoned circuit-side (TunnelKey kept for undo): inactive, not gone.
        assert!(app.circuit.tunnels.get(t_key).is_some_and(|t| !t.active));
        assert!(app
            .wiring
            .active_nodes()
            .all(|(_, n)| !matches!(n.attach, NodeAttach::Tunnel(k) if k == t)));
        assert_eq!(app.selected, None);
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
        let pull = app.place_tunnel(TunnelRole::Pull, GridPos::new(1, 1));
        let feed = app.place_tunnel(TunnelRole::Feed, GridPos::new(2, 2));

        connect_pin_tunnel(&mut app, (inp, PinId::output(0)), pull);
        connect_pin_tunnel(&mut app, (out, PinId::input(0)), feed);
        app.rebuild_circuit();

        let out_key = app.components[out].key;
        assert_eq!(app.circuit.read_output(out_key), Value::Floating);

        // GUI label changed only; circuit.rename_tunnel deliberately NOT called.
        let shared = app.tunnels[pull].label.clone();
        app.tunnels[feed].label = shared;
        app.rebuild_circuit();

        assert_eq!(app.circuit.read_output(out_key), Value::ONE);
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
        app.rebuild_circuit();

        let pan = Vec2::ZERO;
        let rect = selection_screen_rect(GridPos::new(-2, -2), GridPos::new(12, 12), pan);
        let items = app.items_in_rect(rect, pan);
        assert!(items.contains(&Selected::Component(a)));
        assert!(items.contains(&Selected::Component(b)));
        assert!(!items.contains(&Selected::Component(far)));

        app.selected = Some(Selection::Bulk(items));
        app.delete_bulk();

        // Deleted records are tombstoned (inactive), the untouched one stays active.
        assert!(!app.components[a].active);
        assert!(!app.components[b].active);
        assert!(app.components[far].active);
        assert_eq!(app.selected, None);
        // The wire between a and b went with them.
        assert_eq!(app.wiring.active_segments().count(), 0);
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
        app.rebuild_circuit();
        let o_key = app.components[o].key;
        assert_eq!(app.circuit.read_output(o_key), Value::ZERO);

        // Delete the a->g segment (the one touching a's output pin node).
        let seg = app
            .wiring
            .segments
            .iter()
            .find(|(_, s)| {
                matches!(app.wiring.nodes[s.a].attach, NodeAttach::Pin(k, _) if k == a)
                    || matches!(app.wiring.nodes[s.b].attach, NodeAttach::Pin(k, _) if k == a)
            })
            .map(|(k, _)| k)
            .unwrap();
        app.delete_wire(seg);

        // g's input is now Floating -> NOT(Floating) = Floating at the output.
        assert_eq!(app.circuit.read_output(o_key), Value::Floating);
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
        app.rebuild_circuit();

        let seg = app.wiring.active_segments().next().map(|(k, _)| k).unwrap();
        let stack_before = app.history.len();
        app.delete_wire(seg);

        assert_eq!(app.history.len(), stack_before + 1);
        assert!(matches!(
            app.history.last(),
            Some(HistoryEntry::Gui(GuiUndoAction::WiringDelta { .. }))
        ));
    }

    #[test]
    fn test_commit_move_pushes_undo_only_when_position_changed() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let original = app.components[a].grid_pos;
        let stack_before = app.history.len();

        // No movement: nothing pushed.
        app.commit_move(Selected::Component(a), original);
        assert_eq!(app.history.len(), stack_before);

        // Moved: pushes one MoveComponent entry with the correct old_pos.
        app.components[a].grid_pos = GridPos::new(original.x + 3, original.y + 1);
        app.commit_move(Selected::Component(a), original);
        assert_eq!(app.history.len(), stack_before + 1);
        match app.history.last() {
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
        let orig_a = app.components[a].grid_pos;
        let orig_b = app.components[b].grid_pos;
        let stack_before = app.history.len();

        // Mirrors what interact_component_drag's drag_stopped branch does for
        // a Selection::Bulk: every dragged item already moved (one frame at
        // a time, by the same pointer delta - simulated here directly since
        // driving the gesture needs a live egui::Response), then the whole
        // set is committed inside one begin_batch/end_batch.
        app.components[a].grid_pos = GridPos::new(orig_a.x + 3, orig_a.y + 2);
        app.components[b].grid_pos = GridPos::new(orig_b.x + 3, orig_b.y + 2);

        app.history.begin_batch();
        app.commit_move(Selected::Component(a), orig_a);
        app.commit_move(Selected::Component(b), orig_b);
        app.history.end_batch();

        // One batch entry for the whole gesture, not one per item.
        assert_eq!(app.history.len(), stack_before + 1);
        assert!(matches!(app.history.last(), Some(HistoryEntry::Batch(_))));

        // One undo restores every item's original position at once.
        app.undo();
        assert_eq!(app.components[a].grid_pos, orig_a);
        assert_eq!(app.components[b].grid_pos, orig_b);

        // One redo replays the whole move again.
        app.redo();
        assert_eq!(
            app.components[a].grid_pos,
            GridPos::new(orig_a.x + 3, orig_a.y + 2)
        );
        assert_eq!(
            app.components[b].grid_pos,
            GridPos::new(orig_b.x + 3, orig_b.y + 2)
        );
    }

    #[test]
    fn test_drag_grid_pos_excludes_wire_selection() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        assert!(app
            .drag_grid_pos(Selected::Wire(Default::default()), Vec2::ZERO)
            .is_none());
        assert!(app
            .drag_grid_pos(Selected::Component(a), Vec2::ZERO)
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
            &app.components[a].spec.shape(),
            app.components[a].grid_pos,
            PinId::output(0),
        );
        let pb = pin_grid_pos(
            &app.components[b].spec.shape(),
            app.components[b].grid_pos,
            PinId::input(0),
        );
        let elbow = GridPos::new(pa.x + 2, pb.y + 4);
        let delta = app.wiring.add_route(
            &[pa, elbow, pb],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(b, PinId::input(0)),
        );
        app.edit_wiring(delta);
        app.rebuild_circuit();

        let elbow_key = app
            .wiring
            .active_nodes()
            .find(|(_, n)| matches!(n.attach, NodeAttach::Free))
            .map(|(k, _)| k)
            .unwrap();
        let seg_keys: Vec<WireSegKey> = app.wiring.active_segments().map(|(k, _)| k).collect();
        (a, b, elbow_key, seg_keys)
    }

    #[test]
    fn test_free_wire_nodes_dedupes_shared_elbow_and_excludes_pin_nodes() {
        let mut app = OsmilogApp::empty();
        let (_, _, elbow, segs) = place_route_with_elbow(&mut app);
        assert_eq!(segs.len(), 2, "pin -> elbow -> pin is two segments");

        let sels: Vec<Selected> = segs.iter().map(|&s| Selected::Wire(s)).collect();
        let free_nodes = app.free_wire_nodes(&sels);

        // Both segments share the elbow node; it must appear exactly once,
        // and the two Pin-attached endpoints must not appear at all.
        assert_eq!(free_nodes.len(), 1);
        assert_eq!(free_nodes[0].0, elbow);
        assert_eq!(free_nodes[0].1, app.wiring.nodes[elbow].pos);
    }

    #[test]
    fn test_bulk_drag_batch_restores_free_wire_node_alongside_components() {
        let mut app = OsmilogApp::empty();
        let (a, b, elbow, _segs) = place_route_with_elbow(&mut app);
        let orig_a = app.components[a].grid_pos;
        let orig_b = app.components[b].grid_pos;
        let orig_elbow = app.wiring.nodes[elbow].pos;

        // What interact_component_drag does for one drag frame of a bulk
        // selection covering both components and the whole wire run: move
        // every component (syncing its pin-attached nodes) and every Free
        // elbow node by the same delta.
        let new_a = GridPos::new(orig_a.x + 3, orig_a.y + 2);
        let new_b = GridPos::new(orig_b.x + 3, orig_b.y + 2);
        let new_elbow = GridPos::new(orig_elbow.x + 3, orig_elbow.y + 2);
        app.components[a].grid_pos = new_a;
        app.sync_component_wire_nodes(a);
        app.components[b].grid_pos = new_b;
        app.sync_component_wire_nodes(b);
        app.wiring.nodes[elbow].pos = new_elbow;

        // Wire geometry moved as a whole - the elbow isn't left behind.
        assert_eq!(app.wiring.nodes[elbow].pos, new_elbow);

        // drag_stopped: commit every moved item and node as one undo batch.
        let stack_before = app.history.len();
        app.history.begin_batch();
        app.commit_move(Selected::Component(a), orig_a);
        app.commit_move(Selected::Component(b), orig_b);
        app.commit_wire_node_move(elbow, orig_elbow);
        app.history.end_batch();
        assert_eq!(app.history.len(), stack_before + 1);
        assert!(matches!(app.history.last(), Some(HistoryEntry::Batch(_))));

        // One undo restores the components AND the elbow together.
        app.undo();
        assert_eq!(app.components[a].grid_pos, orig_a);
        assert_eq!(app.components[b].grid_pos, orig_b);
        assert_eq!(app.wiring.nodes[elbow].pos, orig_elbow);

        // One redo replays the whole move again.
        app.redo();
        assert_eq!(app.components[a].grid_pos, new_a);
        assert_eq!(app.components[b].grid_pos, new_b);
        assert_eq!(app.wiring.nodes[elbow].pos, new_elbow);
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
        let comp_key = app.components[g].key;
        assert!(app.history.can_undo());
        assert!(!app.history.can_redo());
        assert!(app.components[g].active);
        assert!(app.circuit.components[comp_key].active);

        app.undo();
        assert!(!app.components[g].active, "record tombstoned by undo");
        assert!(
            !app.circuit.components[comp_key].active,
            "circuit component deactivated by undo"
        );
        assert!(app.history.can_redo());
        assert!(!app.history.can_undo());

        app.redo();
        assert!(app.components[g].active);
        assert!(app.circuit.components[comp_key].active);
        assert!(app.history.can_undo());
        assert!(!app.history.can_redo());
    }

    #[test]
    fn undo_redo_wire_draw_round_trips_connectivity() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let o = place(&mut app, ComponentSpec::Output);
        app.commit_wire_route(
            vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(o, PinId::input(0)),
        );
        let o_key = app.components[o].key;
        assert_eq!(app.circuit.read_output(o_key), Value::ONE);
        assert_eq!(app.wiring.groups().len(), 1);

        app.undo();
        assert!(
            app.wiring.groups().iter().all(|grp| grp.pins.len() < 2),
            "wire removed: no group ties both pins together"
        );
        assert_eq!(app.circuit.read_output(o_key), Value::Floating);

        app.redo();
        assert_eq!(app.wiring.groups().len(), 1);
        assert_eq!(app.circuit.read_output(o_key), Value::ONE);
    }

    #[test]
    fn undo_redo_delete_component_restores_wire_and_value() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let o = place(&mut app, ComponentSpec::Output);
        app.commit_wire_route(
            vec![GridPos::new(0, 0), GridPos::new(10, 0)],
            NodeAttach::Pin(a, PinId::output(0)),
            NodeAttach::Pin(o, PinId::input(0)),
        );
        let o_key = app.components[o].key;
        assert_eq!(app.circuit.read_output(o_key), Value::ONE);

        app.delete_component(a);
        assert!(!app.components[a].active);
        assert_eq!(app.circuit.read_output(o_key), Value::Floating);

        app.undo();
        assert!(app.components[a].active);
        let a_key = app.components[a].key;
        assert!(app.circuit.components[a_key].active);
        assert_eq!(
            app.circuit.read_output(o_key),
            Value::ONE,
            "wire nodes and driving input restored"
        );

        app.redo();
        assert!(!app.components[a].active);
        assert_eq!(app.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn undo_redo_reconfigure_restores_def_and_key() {
        let mut app = OsmilogApp::empty();
        let g = place(&mut app, and2());
        let old_key = app.components[g].key;
        app.reconfigure_component(
            g,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        assert!(matches!(
            app.components[g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::Not,
                ..
            })
        ));

        app.undo();
        assert!(matches!(
            app.components[g].spec,
            ComponentSpec::Gate(Gate {
                op: GateOp::And,
                n_inputs: 2,
                ..
            })
        ));
        assert_eq!(app.components[g].key, old_key, "old CompKey restored");

        app.redo();
        assert!(matches!(
            app.components[g].spec,
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
        let original = app.components[a].grid_pos;
        let moved = GridPos::new(original.x + 4, original.y + 2);
        app.components[a].grid_pos = moved;
        app.commit_move(Selected::Component(a), original);

        app.undo();
        assert_eq!(app.components[a].grid_pos, original);

        app.redo();
        assert_eq!(app.components[a].grid_pos, moved);
    }

    #[test]
    fn new_edit_after_undo_clears_redo() {
        let mut app = OsmilogApp::empty();
        place(&mut app, and2());
        app.undo();
        assert!(app.history.can_redo());
        // A fresh edit invalidates the redo branch.
        place(&mut app, ComponentSpec::Output);
        assert!(!app.history.can_redo());
    }

    #[test]
    fn undo_redo_tunnel_rename_restores_label() {
        let mut app = OsmilogApp::empty();
        let t = app.place_tunnel(TunnelRole::Feed, GridPos::new(0, 0));
        let tunnel_key = app.tunnels[t].key;
        let original = app.tunnels[t].label.clone();

        // Simulate a rename commit: record label change live, then the batched
        // Sim rename + record-label undo (mirrors show_tunnel_properties).
        app.tunnels[t].label = "RENAMED".to_string();
        app.history.begin_batch();
        app.apply(Command::RenameTunnel {
            tunnel: tunnel_key,
            new_label: "RENAMED".to_string(),
        });
        app.history.push_gui(GuiUndoAction::SetTunnelLabel {
            key: t,
            label: original.clone(),
        });
        app.history.end_batch();

        app.undo();
        assert_eq!(app.tunnels[t].label, original);
        assert_eq!(app.circuit.tunnels[tunnel_key].label, original);

        app.redo();
        assert_eq!(app.tunnels[t].label, "RENAMED");
        assert_eq!(app.circuit.tunnels[tunnel_key].label, "RENAMED");
    }

    #[test]
    fn test_copy_single_component_then_paste_creates_offset_copy() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        let original = app.components[a].grid_pos;

        app.selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();
        assert!(!app.clipboard.is_empty());

        app.paste_clipboard();

        assert_eq!(app.active_components().count(), 2);
        let pasted = app
            .active_components()
            .find(|(k, _)| *k != a)
            .map(|(k, _)| k)
            .unwrap();
        assert_eq!(
            app.components[pasted].grid_pos,
            GridPos::new(original.x + 2, original.y + 2)
        );
        assert_eq!(
            app.selected,
            Some(Selection::Single(Selected::Component(pasted)))
        );
    }

    #[test]
    fn test_paste_after_undo_of_original_still_works() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        app.selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();

        // Undo the original placement: it's now tombstoned.
        app.undo();
        assert!(!app.components[a].active);

        app.paste_clipboard();

        // The paste is independent of the now-tombstoned original.
        let pasted = app
            .active_components()
            .find(|(k, _)| *k != a)
            .map(|(k, _)| k);
        assert!(pasted.is_some());
        assert_eq!(app.active_components().count(), 1);
    }

    #[test]
    fn test_paste_after_editing_original_is_unaffected() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        let original_pos = app.components[a].grid_pos;
        app.selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();

        // Move the original after copying.
        app.components[a].grid_pos = GridPos::new(100, 100);

        app.paste_clipboard();

        // The pasted copy reflects the pre-edit snapshot, offset from the
        // original's position at copy time - not its current position.
        let pasted = app
            .active_components()
            .find(|(k, _)| *k != a)
            .map(|(k, _)| k)
            .unwrap();
        assert_eq!(
            app.components[pasted].grid_pos,
            GridPos::new(original_pos.x + 2, original_pos.y + 2)
        );
    }

    #[test]
    fn test_paste_normalizes_selection_to_single_for_one_item() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Output);
        app.selected = Some(Selection::Single(Selected::Component(a)));
        app.copy_selection();
        app.paste_clipboard();
        assert!(matches!(app.selected, Some(Selection::Single(_))));
    }

    #[test]
    fn test_paste_is_one_undo_batch() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentSpec::Input(Input { bits: 1, width: 1 }));
        let b = place(&mut app, ComponentSpec::Output);
        connect_pins(&mut app, (a, PinId::output(0)), (b, PinId::input(0)));
        app.rebuild_circuit();
        let seg = app.wiring.active_segments().next().unwrap().0;

        app.selected = Some(Selection::Bulk(vec![
            Selected::Component(a),
            Selected::Component(b),
            Selected::Wire(seg),
        ]));
        app.copy_selection();
        app.paste_clipboard();
        assert_eq!(app.active_components().count(), 4);
        assert_eq!(app.wiring.active_segments().count(), 2);

        // One undo removes the entire pasted batch (components + wiring).
        app.undo();
        assert_eq!(app.active_components().count(), 2);
        assert_eq!(app.wiring.active_segments().count(), 1);
    }

    #[test]
    fn test_paste_noop_when_clipboard_empty() {
        let mut app = OsmilogApp::empty();
        place(&mut app, ComponentSpec::Output);
        assert!(app.clipboard.is_empty());

        let before = app.components.len();
        app.paste_clipboard();
        assert_eq!(app.components.len(), before);
        assert_eq!(app.selected, None);
    }

    #[test]
    fn test_next_tick_schedule_is_frame_rate_independent() {
        let interval = 0.2;

        // Interval not elapsed yet: no tick.
        assert_eq!(next_tick_schedule(0.1, 0.0, interval), None);

        // Dense frames (mouse moving): a frame lands just past the boundary.
        // The reference advances by exactly one interval, NOT to `now`, so the
        // small overshoot doesn't accumulate into the cadence.
        assert_eq!(next_tick_schedule(0.21, 0.0, interval), Some(0.2));
        assert_eq!(next_tick_schedule(0.216, 0.0, interval), Some(0.2));

        // Sparse frames still within one interval of slack: same steady step,
        // so idle and moving-mouse converge on the same average rate.
        assert_eq!(next_tick_schedule(0.39, 0.0, interval), Some(0.2));

        // Fell more than one interval behind (idle/suspended): resync to `now`
        // and drop the missed ticks rather than firing a burst.
        assert_eq!(next_tick_schedule(5.0, 0.0, interval), Some(5.0));
    }

    #[test]
    fn test_editing_locked_tracks_run_state() {
        let mut app = OsmilogApp::empty();
        // Stopped (initial) is editable.
        assert!(!app.editing_locked());

        // Both Playing and Paused lock the whole run session.
        app.clock.run = ClockRun::Playing;
        assert!(app.editing_locked());
        app.clock.run = ClockRun::Paused;
        assert!(app.editing_locked());

        // Stop returns to editable.
        app.stop_clock();
        assert_eq!(app.clock.run, ClockRun::Stopped);
        assert!(!app.editing_locked());
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
        app.rebuild_circuit();

        let out_key = app.components[out].key;
        // Register powers on at 0.
        assert_eq!(app.circuit.read_output(out_key), Value::ZERO);

        // A tick with write-enable high latches the data (1).
        app.tick_once();
        assert_eq!(app.circuit.read_output(out_key), Value::ONE);

        // Stop resets the register to 0 and returns to the editable state.
        app.clock.run = ClockRun::Playing;
        app.stop_clock();
        assert_eq!(app.clock.run, ClockRun::Stopped);
        assert_eq!(app.circuit.read_output(out_key), Value::ZERO);
    }
}

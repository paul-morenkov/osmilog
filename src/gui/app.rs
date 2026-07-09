use eframe;
use egui::epaint::{PathShape, PathStroke};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use slotmap::{new_key_type, SlotMap};
use std::collections::HashMap;

use crate::gui::geometry::{snap_to_grid, tunnel_shape, GridPos, GRID_SIZE, LABEL_FONT_SIZE};
use crate::gui::history::History;
use crate::gui::placed_component::PlacedComponent;
use crate::gui::shape::{tessellate_path, ComponentShape, BUBBLE_R};
use crate::gui::theme::Theme;
use crate::gui::wiring::{NodeAttach, WireNode, WireNodeKey, WireSegKey, WireSegment, Wiring};
use crate::io::{
    CircuitFile, ComponentEntry, LoadError, NodeAttachEntry, NodeEntry, SegEntry, TunnelEntry,
    CURRENT_VERSION,
};
use crate::sim::circuit::{Circuit, TunnelKey, TunnelRole};
use crate::sim::command::Command;
use crate::sim::component::{
    Adder, CompKey, Comparator, ComponentSpec, Demux, Divider, Encoder, FanDirection, Gate,
    GateOp, InIdx, Input, Multiplier, Mux, OutIdx, PinId, Reg, Subtractor,
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

// Visual record for a Tunnel (net label / off-page connector). Tunnel lives at the Circuit
// level as its own SlotMap, tied to a net directly rather than via Component
// pins. `label` mirrors circuit::Tunnel.label directly (editing it both
// updates the displayed text and calls circuit.rename_tunnel). Components,
// by contrast, show only hardcoded, non-editable per-type/pin labels (see
// ComponentShape::labels) - Tunnels are the only entity with a user-editable
// label.
pub struct PlacedTunnel {
    pub key: TunnelKey,
    pub label: String,
    pub role: TunnelRole,
    pub grid_pos: GridPos,
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

// ── InteractionMode ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum InteractionMode {
    Idle,
    Placing {
        def: ComponentSpec,
    },
    PlacingTunnel {
        role: TunnelRole,
    },
    // Drawing a wire (Hybrid: drag = quick elbow, click = add a corner). `points`
    // are the committed grid corners (points[0] is the anchor); `start_attach`
    // binds the anchor to a pin/tunnel when the draw began on one; `dragging`
    // distinguishes a drag (finish on release) from a click-polyline (finish on
    // clicking a target, double-click, or Esc).
    WireDraw {
        points: Vec<GridPos>,
        start_attach: NodeAttach,
        cursor: Pos2,
        dragging: bool,
    },
    ComponentDrag {
        key: Selected,
        drag_origin: Pos2,
        original_grid_pos: GridPos,
    },
    // Rubber-band multi-select, entered by dragging from an empty region.
    // `start` is the grid cell the drag began on and `current` tracks the
    // drag's live corner; on release everything inside the box they trace is
    // added to `bulk_selection`. Both are GridPos so the box snaps to the grid
    // like every other placement.
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
    pub selected: Option<Selected>,
    // A multi-item selection produced by BulkSelect. Kept separate from the
    // single `selected` (which drives the properties panel and body-drag): when
    // this is non-empty, Backspace/Delete removes the whole set. Cleared by a
    // click in empty space or Escape.
    pub bulk_selection: Vec<Selected>,
    // Also surfaces File > Save/Load I/O errors, not just settle() errors -
    // both are transient "something went wrong" status shown in the same
    // red label in the menu bar (see the "Menu bar" section of `ui`).
    pub last_settle_error: Option<String>,
    // WASM has no synchronous file dialogs (picking/reading a file is
    // Promise-based), so a load kicked off from the File menu delivers its
    // result here on some later frame instead of returning directly to the
    // click handler that started it - see `apply_pending_load`.
    #[cfg(target_arch = "wasm32")]
    pending_load: crate::io::wasm::PendingLoad,
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
            bulk_selection: Vec::new(),
            last_settle_error: None,
            #[cfg(target_arch = "wasm32")]
            pending_load: crate::io::wasm::new_pending_load(),
        }
    }

    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::empty()
    }

    fn record_settle_result<T>(&mut self, result: Result<T, crate::sim::circuit::SettleError>) {
        match result {
            Ok(_) => self.last_settle_error = None,
            Err(e) => self.last_settle_error = Some(e.to_string()),
        }
    }

    // Applies a Command to the circuit and records its UndoAction into
    // history, in one place - callers use this exactly like
    // circuit.apply() (same CommandOutput, same unwrap_* chaining), never
    // needing to look at the undo data themselves.
    fn apply(&mut self, command: Command) -> crate::sim::command::CommandOutput {
        let (output, undo) = self.circuit.apply_tracked(command);
        self.history.push(undo);
        output
    }

    fn place_component(&mut self, def: ComponentSpec, grid_pos: GridPos) -> PlacedCompKey {
        self.history.begin_batch();
        let comp = def.to_component();
        let key = self.apply(Command::AddComponent(comp)).unwrap_comp();
        self.history.end_batch();
        let pc = PlacedComponent { key, def, grid_pos };
        self.components.insert(pc)
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
            .apply(Command::AddTunnel { label: label.clone(), role })
            .unwrap_tunnel();
        self.history.end_batch();
        let pt = PlacedTunnel {
            key,
            label,
            role,
            grid_pos,
        };
        self.tunnels.insert(pt)
    }

    // Rebuilds every circuit net from the GUI wiring graph. clear_nets() drops
    // all nets while keeping components/tunnels in place, then each connected
    // wire group is replayed as circuit links: its component pins are linked
    // together (fan-out and driver-conflict handling live in Circuit::link /
    // resolve_net) and each tunnel in the group is attached to that net. A
    // wire group with no component pin (tunnels-only, or purely dangling) has
    // no net to attach to and is skipped. Called after any wiring edit.
    fn rebuild_circuit(&mut self) {
        // Nested: a caller (delete_component, reconfigure_component, ...)
        // may already have an outer batch open for its own apply() calls
        // before reaching here; History's depth counter collapses the whole
        // sequence into one undo entry either way.
        self.history.begin_batch();

        // Reconcile each circuit tunnel's label from its GUI record, which is
        // the source of truth. The properties panel updates PlacedTunnel.label
        // live but only commits circuit.rename_tunnel on an explicit Enter, so
        // without this a label changed some other way (clicking away) would
        // leave the circuit grouping the tunnel under its stale label - and its
        // Feed/Pull partner would never join the group. rename_tunnel is a
        // no-op when the label already matches.
        let renames: Vec<(TunnelKey, String)> = self
            .tunnels
            .values()
            .filter(|pt| self.circuit.tunnel_label(pt.key) != Some(pt.label.as_str()))
            .map(|pt| (pt.key, pt.label.clone()))
            .collect();
        for (key, label) in renames {
            self.apply(Command::RenameTunnel { tunnel: key, new_label: label });
        }

        self.apply(Command::ClearNets);
        for group in self.wiring.groups() {
            // Map GUI PlacedCompKeys to live circuit CompKeys; a stale key
            // (component already gone) is dropped.
            let pins: Vec<(CompKey, PinId)> = group
                .pins
                .iter()
                .filter_map(|&(pck, pin)| self.components.get(pck).map(|pc| (pc.key, pin)))
                .collect();
            let Some(&(anchor_comp, anchor_pin)) = pins.first() else {
                continue; // no component pin: nothing to drive a net
            };
            for &(comp, pin) in &pins[1..] {
                self.apply(Command::Link {
                    a: anchor_comp,
                    a_pin: anchor_pin,
                    b: comp,
                    b_pin: pin,
                });
            }
            for &ptk in &group.tunnels {
                if let Some(pt) = self.tunnels.get(ptk) {
                    self.apply(Command::LinkTunnel {
                        tunnel: pt.key,
                        comp: anchor_comp,
                        pin: anchor_pin,
                    });
                }
            }
        }
        self.history.end_batch();
        let result = self.circuit.settle();
        self.record_settle_result(result);
    }

    // Repositions the component's wire-anchor nodes to its current pin grid
    // positions (after a move or reconfigure). Segments to them stretch.
    fn sync_component_wire_nodes(&mut self, pck: PlacedCompKey) {
        let Some(pc) = self.components.get(pck) else {
            return;
        };
        let shape = pc.def.shape();
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
        let mut out = HashMap::new();
        for group in self.wiring.groups() {
            let mut val = Value::Floating;
            for &(pck, pin) in &group.pins {
                if let Some(pc) = self.components.get(pck) {
                    if let Some(nk) = self.circuit.components[pc.key].net_of(pin) {
                        val = self.circuit.nets[nk].value;
                        break;
                    }
                }
            }
            if val == Value::Floating {
                for &ptk in &group.tunnels {
                    if let Some(pt) = self.tunnels.get(ptk) {
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

    // Resolves what lies under a screen position for wiring purposes: the
    // attachment to bind (pin/tunnel/free), the on-grid point to route to, and
    // whether it is a real terminal (pin/tunnel/wire) vs. empty space. Priority:
    // component pin (out, then in), tunnel pin, existing wire node, wire segment,
    // else the snapped cursor cell (empty space, not a terminal).
    fn wire_target_at(&self, pos: Pos2, pan: Vec2) -> (NodeAttach, GridPos, bool) {
        if let Some((pck, pin)) = pin_at_pos(self.components.iter(), pan, pos, PinKind::Output) {
            let gp = pin_grid_pos(
                &self.components[pck].def.shape(),
                self.components[pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some((pck, pin)) = pin_at_pos(self.components.iter(), pan, pos, PinKind::Input) {
            let gp = pin_grid_pos(
                &self.components[pck].def.shape(),
                self.components[pck].grid_pos,
                pin,
            );
            return (NodeAttach::Pin(pck, pin), gp, true);
        }
        if let Some(ptk) = tunnel_pin_at_pos(self.tunnels.iter(), pan, pos) {
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
        // PlacedCompKey/PlacedTunnelKey -> position in the Vec being emitted,
        // so wire nodes can reference components/tunnels by index instead of a
        // slotmap key. Built here, then read when emitting `nodes` below.
        let mut comp_index: HashMap<PlacedCompKey, usize> = HashMap::new();
        let components: Vec<ComponentEntry> = self
            .components
            .iter()
            .enumerate()
            .map(|(i, (pck, pc))| {
                comp_index.insert(pck, i);
                ComponentEntry {
                    def: pc.def.clone(),
                    grid_pos: pc.grid_pos,
                }
            })
            .collect();

        let mut tunnel_index: HashMap<PlacedTunnelKey, usize> = HashMap::new();
        let tunnels: Vec<TunnelEntry> = self
            .tunnels
            .iter()
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
        let nodes: Vec<NodeEntry> = self
            .wiring
            .nodes
            .iter()
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
            .segments
            .values()
            .map(|s| SegEntry {
                a: node_index[&s.a],
                b: node_index[&s.b],
            })
            .collect();

        CircuitFile {
            version: CURRENT_VERSION,
            components,
            tunnels,
            nodes,
            segments,
        }
    }

    // Replaces the current circuit entirely with the one described by
    // `file`. Validates first so a malformed file (e.g. hand-edited with a
    // bad index) is rejected before any existing state is touched, rather
    // than leaving `self` half-overwritten.
    pub fn load_circuit_file(&mut self, file: &CircuitFile) -> Result<(), LoadError> {
        file.validate()?;

        self.circuit = Circuit::new();
        self.components = SlotMap::default();
        self.tunnels = SlotMap::default();
        self.wiring = Wiring::new();
        self.selected = None;
        self.bulk_selection.clear();
        self.mode = InteractionMode::Idle;
        self.last_settle_error = None;

        // File indices -> the freshly placed GUI keys (wiring nodes reference
        // components/tunnels by these).
        let comp_keys: Vec<PlacedCompKey> = file
            .components
            .iter()
            .map(|entry| self.place_component(entry.def.clone(), entry.grid_pos))
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
                })
            })
            .collect();

        for s in &file.segments {
            self.wiring.segments.insert(WireSegment {
                a: node_keys[s.a],
                b: node_keys[s.b],
            });
        }

        self.rebuild_circuit();
        Ok(())
    }

    /// Shows property menu for the currently selected item. ComponentSpec for the UI element is
    /// cloned. If the user edits a property, call `self.reconfigure_component()` with an updated ComponentSpec
    fn show_properties(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.selected else {
            if !self.bulk_selection.is_empty() {
                ui.heading("SELECTION");
                ui.separator();
                ui.label(format!("{} items selected.", self.bulk_selection.len()));
                ui.label("Press Backspace or Delete to remove them.");
            } else {
                ui.label("Click a component or tunnel to select it.");
            }
            return;
        };
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

        // Commit on any focus loss - Enter, Tab, or clicking away - not only
        // Enter. Committing only on Enter left the GUI label (updated above on
        // `changed()`) ahead of the circuit's, so a tunnel renamed by clicking
        // away stayed grouped under its old label and its Feed partner read
        // Floating. (rebuild_circuit also reconciles as a backstop.)
        if response.lost_focus() {
            self.history.begin_batch();
            self.apply(Command::RenameTunnel {
                tunnel: tunnel_key,
                new_label: label.clone(),
            });
            self.history.end_batch();
            self.tunnels[key].label = label;
            let result = self.circuit.settle();
            self.record_settle_result(result);
        }
    }

    fn show_component_properties(&mut self, key: PlacedCompKey, ui: &mut egui::Ui) {
        let pc = &self.components[key];
        let comp_key = pc.key;

        ui.heading(pc.def.label());
        ui.separator();

        let def = pc.def.clone();
        match def {
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
            ComponentSpec::Reg(Reg { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentSpec::Reg(Reg { data_width }));
                }

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

                // Apply width/arms bookkeeping before rendering bit rows below, so a
                // shrink is reflected in the same frame. Truncating arm_bits below
                // `arms` correctly drops any bits assigned to a removed arm - nothing
                // else references that arm index, since arm-major storage has nothing
                // else to clean up.
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

    // Swaps a placed component's parameters. The PlacedCompKey is stable and the
    // wiring binds to it, so attached wires survive automatically - we only drop
    // wire nodes for pins the new arity no longer has, re-sync the surviving
    // anchors to the new pin positions, then rebuild the circuit from the wiring.
    fn reconfigure_component(&mut self, pc_key: PlacedCompKey, new_def: ComponentSpec) {
        self.history.begin_batch();
        let old_key = self.components[pc_key].key;
        let grid_pos = self.components[pc_key].grid_pos;

        let new_comp = new_def.to_component();
        let new_n_in = new_comp.pins.inputs.len();
        let new_n_out = new_comp.pins.outputs.len();

        self.apply(Command::RemoveComponent(old_key));
        let new_key = self.apply(Command::AddComponent(new_comp)).unwrap_comp();
        self.components[pc_key] = PlacedComponent {
            key: new_key,
            def: new_def,
            grid_pos,
        };

        self.wiring.prune_stale_pins(pc_key, new_n_in, new_n_out);
        self.sync_component_wire_nodes(pc_key);
        self.rebuild_circuit();
        self.history.end_batch();
        self.selected = Some(Selected::Component(pc_key));
    }

    // Removes a placed component: drop it from the circuit and its wire nodes
    // from the wiring graph, then rebuild the circuit's nets from what remains.
    fn delete_component(&mut self, key: PlacedCompKey) {
        self.history.begin_batch();
        let comp_key = self.components[key].key;
        self.apply(Command::RemoveComponent(comp_key));
        self.wiring.remove_component_nodes(key);
        self.components.remove(key);
        if self.selected == Some(Selected::Component(key)) {
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
        self.wiring.remove_tunnel_nodes(key);
        self.tunnels.remove(key);
        if self.selected == Some(Selected::Tunnel(key)) {
            self.selected = None;
        }
        self.rebuild_circuit();
        self.history.end_batch();
    }

    // Removes a single wire segment; the wiring graph handles orphan cleanup and
    // any net split, then the circuit is rebuilt.
    fn delete_wire(&mut self, seg: WireSegKey) {
        self.wiring.delete_segment(seg);
        if self.selected == Some(Selected::Wire(seg)) {
            self.selected = None;
        }
        self.rebuild_circuit();
    }

    // True if `sel` is either the single selection or part of the bulk
    // selection, i.e. it should be drawn highlighted.
    fn is_highlighted(&self, sel: Selected) -> bool {
        self.selected == Some(sel) || self.bulk_selection.contains(&sel)
    }

    // Every component, tunnel, and wire segment fully contained in `rect`
    // (screen space). Used by BulkSelect to turn a rubber-band box into a
    // selection: a component/tunnel counts when its whole bounding rect is
    // inside, a wire when both its endpoints are.
    fn items_in_rect(&self, rect: Rect, pan: Vec2) -> Vec<Selected> {
        let mut out = Vec::new();
        for (key, pc) in &self.components {
            if rect.contains_rect(component_bounding_rect(pc, pan)) {
                out.push(Selected::Component(key));
            }
        }
        for (key, pt) in &self.tunnels {
            if rect.contains_rect(tunnel_bounding_rect(pt, pan)) {
                out.push(Selected::Tunnel(key));
            }
        }
        for (key, seg) in self.wiring.segments.iter() {
            let a = grid_to_screen(self.wiring.nodes[seg.a].pos, pan);
            let b = grid_to_screen(self.wiring.nodes[seg.b].pos, pan);
            if rect.contains(a) && rect.contains(b) {
                out.push(Selected::Wire(key));
            }
        }
        out
    }

    // Removes everything in `bulk_selection` in one shot: wire segments first,
    // then components/tunnels (whose removal also drops their own wire nodes).
    // Each removal is guarded by an existence check because deleting a component
    // can take a wire in the same set with it, and rebuilds the circuit once at
    // the end rather than per item.
    fn delete_bulk(&mut self) {
        self.history.begin_batch();
        let items = std::mem::take(&mut self.bulk_selection);
        for sel in &items {
            if let Selected::Wire(seg) = *sel {
                if self.wiring.segments.contains_key(seg) {
                    self.wiring.delete_segment(seg);
                }
            }
        }
        for sel in &items {
            match *sel {
                Selected::Component(key) => {
                    if let Some(pc) = self.components.get(key) {
                        let comp_key = pc.key;
                        self.apply(Command::RemoveComponent(comp_key));
                        self.wiring.remove_component_nodes(key);
                        self.components.remove(key);
                    }
                }
                Selected::Tunnel(key) => {
                    if let Some(pt) = self.tunnels.get(key) {
                        let tunnel_key = pt.key;
                        self.apply(Command::RemoveTunnel(tunnel_key));
                        self.wiring.remove_tunnel_nodes(key);
                        self.tunnels.remove(key);
                    }
                }
                Selected::Wire(_) => {}
            }
        }
        if let Some(sel) = self.selected {
            if items.contains(&sel) {
                self.selected = None;
            }
        }
        self.rebuild_circuit();
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
}

impl eframe::App for OsmilogApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_arch = "wasm32")]
        self.apply_pending_load();

        if ctx.input(|i| i.viewport().close_requested()) {
            #[cfg(not(target_arch = "wasm32"))]
            std::process::exit(0);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let theme = Theme::from_visuals(ui.visuals());

        // ── Menu bar ──────────────────────────────────────────────────────
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
                                crate::io::wasm::trigger_download("circuit.json", &json);
                            }
                            Err(e) => self.last_settle_error = Some(format!("save failed: {e}")),
                        }
                        ui.close();
                    }
                    if ui.button("Load").clicked() {
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
                                    def: ComponentSpec::Gate(Gate {
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
                            def: ComponentSpec::Input(Input { bits: 0, width: 1 }),
                        };
                        ui.close();
                    }
                    if ui.button("Output").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentSpec::Output,
                        };
                        ui.close();
                    }

                    ui.menu_button("Plexers", |ui| {
                        if ui.button("Mux").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Mux(Mux {
                                    data_width: 1,
                                    sel_width: 1,
                                }),
                            };
                            ui.close();
                        }
                        if ui.button("Demux").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Demux(Demux {
                                    data_width: 1,
                                    sel_width: 1,
                                }),
                            };
                            ui.close();
                        }
                        if ui.button("Splitter").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Splitter {
                                    width: 2,
                                    arm_bits: vec![vec![0], vec![1]],
                                    direction: FanDirection::Right,
                                },
                            };
                            ui.close();
                        }
                        if ui.button("Encoder").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Encoder(Encoder { sel_width: 1 }),
                            };
                            ui.close();
                        }
                    });
                    ui.menu_button("Arithmetic", |ui| {
                        if ui.button("Adder").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Adder(Adder { data_width: 1 }),
                            };
                            ui.close();
                        }
                        if ui.button("Subtractor").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Subtractor(Subtractor { data_width: 1 }),
                            };
                            ui.close();
                        }
                        if ui.button("Multiplier").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Multiplier(Multiplier { data_width: 1 }),
                            };
                            ui.close();
                        }
                        if ui.button("Divider").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Divider(Divider { data_width: 1 }),
                            };
                            ui.close();
                        }
                        if ui.button("Comparator").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Comparator(Comparator { data_width: 1 }),
                            };
                            ui.close();
                        }
                    });
                    ui.menu_button("Memory", |ui| {
                        if ui.button("Register").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentSpec::Reg(Reg { data_width: 1 }),
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
                if ui.button("Tick Clock").clicked() {
                    self.history.begin_batch();
                    let result = self.apply(Command::TickClock).unwrap_settle();
                    self.history.end_batch();
                    self.record_settle_result(result);
                }
                if let Some(err) = &self.last_settle_error {
                    ui.colored_label(theme.error_text, err);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("v{APP_VERSION} ({GIT_SHA})"));
                });
            })
        });

        // ── Properties panel ──────────────────────────────────────────────
        egui::Panel::left("properties")
            .min_size(200.0)
            .resizable(true)
            .show(ui, |ui| {
                self.show_properties(ui);
            });

        // ── Canvas ────────────────────────────────────────────────────────
        {
            let (response, painter) =
                ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
            let clip_rect = painter.clip_rect();
            let pan = self.pan;

            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                match &self.mode {
                    InteractionMode::ComponentDrag {
                        key,
                        original_grid_pos,
                        ..
                    } => {
                        let (key, original_grid_pos) = (*key, *original_grid_pos);
                        match key {
                            Selected::Component(k) => {
                                self.components[k].grid_pos = original_grid_pos
                            }
                            Selected::Tunnel(k) => self.tunnels[k].grid_pos = original_grid_pos,
                            Selected::Wire(_) => {}
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
                        self.wiring
                            .add_route(&points, start_attach, NodeAttach::Free);
                        self.rebuild_circuit();
                    }
                    // BulkSelect: Esc cancels the in-progress rubber-band (the
                    // trailing reset to Idle handles it) alongside clearing any
                    // existing bulk selection below.
                    _ => {}
                }
                self.bulk_selection.clear();
                self.mode = InteractionMode::Idle;
            }

            // Backspace/Delete removes the current selection. A non-empty bulk
            // selection takes priority and is removed as a whole. Guard on widget
            // focus so a Backspace aimed at the tunnel-label text field (or any
            // focused widget) edits text instead of deleting.
            let editing_text = ctx.memory(|m| m.focused().is_some());
            let delete_pressed = ctx
                .input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete));
            if delete_pressed && !editing_text {
                if !self.bulk_selection.is_empty() {
                    self.delete_bulk();
                } else if let Some(sel) = self.selected {
                    match sel {
                        Selected::Component(k) => self.delete_component(k),
                        Selected::Tunnel(k) => self.delete_tunnel(k),
                        Selected::Wire(seg) => self.delete_wire(seg),
                    }
                }
            }

            painter.rect_filled(clip_rect, 0.0, theme.canvas_bg);
            draw_grid(&painter, clip_rect, pan, theme);

            // Draw wire segments. Colour comes from each connected group's net
            // value: any component pin / tunnel on the group resolves (live) to
            // that net's Value; a dangling group (no endpoints) is Floating.
            let node_value = self.wire_node_values();
            for (seg_key, seg) in self.wiring.segments.iter() {
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
            for (nk, node) in self.wiring.nodes.iter() {
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
            for (pc_key, pc) in &self.components {
                let is_selected = self.is_highlighted(Selected::Component(pc_key));
                draw_component(&painter, pc, pan, &self.circuit, is_selected, theme);
            }

            // Draw tunnels
            for (pt_key, pt) in &self.tunnels {
                let is_selected = self.is_highlighted(Selected::Tunnel(pt_key));
                draw_tunnel(&painter, pt, pan, &self.circuit, is_selected, theme);
            }

            let pointer = response
                .interact_pointer_pos()
                .or_else(|| ctx.pointer_hover_pos());

            // Mode-specific interaction
            let mode = self.mode.clone();
            match mode {
                InteractionMode::Idle => {
                    // Hover reticle: hovering over a wire (but not a pin) shows
                    // where a branch would tap the wire.
                    if let Some(pos) = pointer {
                        if pin_at_pos(self.components.iter(), pan, pos, PinKind::Output).is_none()
                            && pin_at_pos(self.components.iter(), pan, pos, PinKind::Input)
                                .is_none()
                            && tunnel_pin_at_pos(self.tunnels.iter(), pan, pos).is_none()
                        {
                            if let Some((_, gp)) = self.wiring.segment_at_pos(pos, pan) {
                                draw_reticle(&painter, grid_to_screen(gp, pan), theme);
                            }
                        }
                    }

                    if response.drag_started() {
                        let origin = ctx.input(|i| i.pointer.press_origin());
                        if let Some(pos) = origin {
                            if let Some((attach, gp)) = self.wire_start_at(pos, pan) {
                                // Drag from a pin / tunnel / existing wire → draw
                                // a wire (quick elbow, committed on release).
                                self.mode = InteractionMode::WireDraw {
                                    points: vec![gp],
                                    start_attach: attach,
                                    cursor: pos,
                                    dragging: true,
                                };
                            } else if let Some((sel, grid_pos)) = self.selected.and_then(|sel| {
                                // Selected component/tunnel body drag → move it,
                                // but only when the drag actually began inside its
                                // bounding rect.
                                let rect_grid = match sel {
                                    Selected::Component(key) => self
                                        .components
                                        .get(key)
                                        .map(|pc| (component_bounding_rect(pc, pan), pc.grid_pos)),
                                    Selected::Tunnel(key) => self
                                        .tunnels
                                        .get(key)
                                        .map(|pt| (tunnel_bounding_rect(pt, pan), pt.grid_pos)),
                                    Selected::Wire(_) => None,
                                };
                                rect_grid
                                    .filter(|(rect, _)| rect.contains(pos))
                                    .map(|(_, grid_pos)| (sel, grid_pos))
                            }) {
                                self.mode = InteractionMode::ComponentDrag {
                                    key: sel,
                                    drag_origin: pos,
                                    original_grid_pos: grid_pos,
                                };
                            } else {
                                // Drag from empty space → rubber-band bulk select.
                                let gp = snap_to_grid(pos, pan);
                                self.selected = None;
                                self.bulk_selection.clear();
                                self.mode = InteractionMode::BulkSelect {
                                    start: gp,
                                    current: gp,
                                };
                            }
                        }
                    }

                    if response.clicked() {
                        if let Some(pos) = pointer {
                            // Any click ends a bulk selection ("click away").
                            self.bulk_selection.clear();
                            // A click starts a polyline only from a pin/tunnel;
                            // clicking a bare wire selects it instead (branching
                            // off a wire is a drag gesture, handled above).
                            let pin_start = self.wire_start_at(pos, pan).filter(|(a, _)| {
                                matches!(a, NodeAttach::Pin(..) | NodeAttach::Tunnel(_))
                            });
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
                                    .find(|(_k, pc)| component_bounding_rect(pc, pan).contains(pos))
                                    .map(|(k, _)| Selected::Component(k));
                                let maybe_tunnel = self
                                    .tunnels
                                    .iter()
                                    .find(|(_k, pt)| tunnel_bounding_rect(pt, pan).contains(pos))
                                    .map(|(k, _)| Selected::Tunnel(k));
                                let maybe_wire = self
                                    .wiring
                                    .segment_at_pos(pos, pan)
                                    .map(|(seg, _)| Selected::Wire(seg));
                                self.selected = maybe_comp.or(maybe_tunnel).or(maybe_wire);
                            }
                        }
                    }
                }

                InteractionMode::Placing { def } => {
                    if let Some(pos) = pointer {
                        let gp = snap_to_grid(pos, pan);
                        draw_ghost(&painter, &def, gp, pan, theme);
                    }
                    if response.clicked() {
                        if let Some(pos) = pointer {
                            let gp = snap_to_grid(pos, pan);
                            self.place_component(def, gp);
                            self.mode = InteractionMode::Idle;
                        }
                    }
                }

                InteractionMode::PlacingTunnel { role } => {
                    if let Some(pos) = pointer {
                        let gp = snap_to_grid(pos, pan);
                        draw_tunnel_ghost(&painter, role, gp, pan, theme);
                    }
                    if response.clicked() {
                        if let Some(pos) = pointer {
                            let gp = snap_to_grid(pos, pan);
                            self.place_tunnel(role, gp);
                            self.mode = InteractionMode::Idle;
                        }
                    }
                }

                InteractionMode::WireDraw {
                    points,
                    start_attach,
                    cursor,
                    dragging,
                } => {
                    let end = pointer.unwrap_or(cursor);
                    let (drop_attach, drop_gp, terminal) = self.wire_target_at(end, pan);

                    // Preview: committed segments, then the pending elbow from the
                    // last committed corner to the (snapped) drop point.
                    let preview = Stroke::new(WIRE_THICKNESS_THIN, theme.wire_drag_preview);
                    for w in points.windows(2) {
                        painter.line_segment(
                            [grid_to_screen(w[0], pan), grid_to_screen(w[1], pan)],
                            preview,
                        );
                    }
                    let pending = route_elbow(*points.last().unwrap(), drop_gp);
                    let mut prev = *points.last().unwrap();
                    for p in &pending {
                        painter.line_segment(
                            [grid_to_screen(prev, pan), grid_to_screen(*p, pan)],
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
                        if response.drag_stopped() {
                            let mut route = points.clone();
                            route.extend(pending);
                            self.wiring.add_route(&route, start_attach, drop_attach);
                            self.rebuild_circuit();
                            self.mode = InteractionMode::Idle;
                        }
                    } else {
                        // Click-polyline: a click on a terminal (or a double-click)
                        // finishes; any other click drops a corner and continues.
                        let mut next_points = points.clone();
                        let mut finished = false;
                        if response.double_clicked() {
                            next_points.extend(pending.clone());
                            self.wiring
                                .add_route(&next_points, start_attach, NodeAttach::Free);
                            finished = true;
                        } else if response.clicked() {
                            next_points.extend(pending.clone());
                            if terminal {
                                self.wiring
                                    .add_route(&next_points, start_attach, drop_attach);
                                finished = true;
                            }
                        }
                        if finished {
                            self.rebuild_circuit();
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

                InteractionMode::ComponentDrag {
                    key,
                    drag_origin,
                    original_grid_pos,
                } => {
                    if let Some(pos) = pointer {
                        let delta_x = ((pos.x - drag_origin.x) / GRID_SIZE).round() as i32;
                        let delta_y = ((pos.y - drag_origin.y) / GRID_SIZE).round() as i32;
                        let new_grid_pos = GridPos::new(
                            original_grid_pos.x + delta_x,
                            original_grid_pos.y + delta_y,
                        );
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
                    if response.drag_stopped() {
                        self.mode = InteractionMode::Idle;
                    }
                }

                InteractionMode::BulkSelect { start, current } => {
                    // Track the live corner, then paint the rubber-band box.
                    let current = pointer.map(|p| snap_to_grid(p, pan)).unwrap_or(current);
                    let rect = selection_screen_rect(start, current, pan);
                    let c = theme.outline_selected;
                    painter.rect_filled(
                        rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 24),
                    );
                    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, c), StrokeKind::Inside);

                    // Finish on release. The `!dragged` guard also recovers from a
                    // flick released the same frame it started (drag_stopped never
                    // fires in the BulkSelect arm then), so the mode can't stick.
                    if response.drag_stopped() || !response.dragged() {
                        self.bulk_selection = self.items_in_rect(rect, pan);
                        self.mode = InteractionMode::Idle;
                    } else {
                        self.mode = InteractionMode::BulkSelect { start, current };
                    }
                }
            }
        }
    }
}

// ── Geometry ─────────────────────────────────────────────────────────────────

fn component_bounding_rect(pc: &PlacedComponent, pan: Vec2) -> Rect {
    let size = pc.def.size();
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

// Takes an already-computed ComponentShape rather than a &PlacedComponent so
// callers that need multiple pins from the same component (draw_component,
// pin_at_pos) compute shape() once and reuse it, instead of each call
// redundantly rebuilding the whole shape (outline/anchors/bubbles Vecs)
// just to read one anchor.
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
    let hit_r = PIN_RADIUS * 2.0;
    for (key, pc) in components {
        let shape = pc.def.shape();
        match kind {
            PinKind::Output => {
                for i in 0..pc.def.n_outputs() {
                    let pp = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::output(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((key, PinId::output(i as u8)));
                    }
                }
            }
            PinKind::Input => {
                for i in 0..pc.def.n_inputs() {
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
    let shape = pc.def.shape();
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

    for i in 0..pc.def.n_inputs() {
        let pos = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::input(i as u8));
        let val = circuit.components[pc.key].pins.inputs[i]
            .map(|nk| circuit.nets[nk].value)
            .unwrap_or(Value::Floating);
        painter.circle_filled(pos, PIN_RADIUS, value_stroke(theme, val).color);
    }
    for i in 0..pc.def.n_outputs() {
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

fn draw_ghost(painter: &Painter, def: &ComponentSpec, grid_pos: GridPos, pan: Vec2, theme: Theme) {
    let shape = def.shape();
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
    use crate::sim::component::GateOp;

    fn place(app: &mut OsmilogApp, def: ComponentSpec) -> PlacedCompKey {
        app.place_component(def, GridPos::new(0, 0))
    }

    // Insert a wire (one segment) between two component pins, positioned at each
    // pin's grid cell, and return the two node keys.
    fn connect_pins(app: &mut OsmilogApp, a: (PlacedCompKey, PinId), b: (PlacedCompKey, PinId)) {
        let pa = pin_grid_pos(
            &app.components[a.0].def.shape(),
            app.components[a.0].grid_pos,
            a.1,
        );
        let pb = pin_grid_pos(
            &app.components[b.0].def.shape(),
            app.components[b.0].grid_pos,
            b.1,
        );
        let na = app.wiring.nodes.insert(WireNode {
            pos: pa,
            attach: NodeAttach::Pin(a.0, a.1),
        });
        let nb = app.wiring.nodes.insert(WireNode {
            pos: pb,
            attach: NodeAttach::Pin(b.0, b.1),
        });
        app.wiring.segments.insert(WireSegment { a: na, b: nb });
    }

    // Insert a wire (one segment) between a component pin and a tunnel.
    fn connect_pin_tunnel(app: &mut OsmilogApp, c: (PlacedCompKey, PinId), ptk: PlacedTunnelKey) {
        let pc = pin_grid_pos(
            &app.components[c.0].def.shape(),
            app.components[c.0].grid_pos,
            c.1,
        );
        let pt = tunnel_pin_grid(&app.tunnels[ptk]);
        let nc = app.wiring.nodes.insert(WireNode {
            pos: pc,
            attach: NodeAttach::Pin(c.0, c.1),
        });
        let nt = app.wiring.nodes.insert(WireNode {
            pos: pt,
            attach: NodeAttach::Tunnel(ptk),
        });
        app.wiring.segments.insert(WireSegment { a: nc, b: nt });
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
            .find(|pc| matches!(pc.def, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.circuit.read_output(loaded_out_key), Value::ONE);
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
            .find(|pc| matches!(pc.def, ComponentSpec::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.circuit.read_output(loaded_out_key), Value::ONE);
    }

    #[test]
    fn test_load_circuit_file_rejects_bad_component_index() {
        let file = CircuitFile {
            version: CURRENT_VERSION,
            components: vec![ComponentEntry {
                def: ComponentSpec::Output,
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
        app.selected = Some(Selected::Component(g));

        app.delete_component(g);

        assert!(!app.components.contains_key(g));
        assert!(app.circuit.components.get(g_key).is_none());
        // No wire node references the deleted component; orphan neighbours were
        // cleaned up too, leaving no segments.
        assert!(app
            .wiring
            .nodes
            .values()
            .all(|n| !matches!(n.attach, NodeAttach::Pin(k, _) if k == g)));
        assert_eq!(app.wiring.segments.len(), 0);
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
        app.selected = Some(Selected::Tunnel(t));

        app.delete_tunnel(t);

        assert!(!app.tunnels.contains_key(t));
        assert!(app.circuit.tunnels.get(t_key).is_none());
        assert!(app
            .wiring
            .nodes
            .values()
            .all(|n| !matches!(n.attach, NodeAttach::Tunnel(k) if k == t)));
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

        app.bulk_selection = items;
        app.delete_bulk();

        assert!(!app.components.contains_key(a));
        assert!(!app.components.contains_key(b));
        assert!(app.components.contains_key(far));
        assert!(app.bulk_selection.is_empty());
        // The wire between a and b went with them.
        assert_eq!(app.wiring.segments.len(), 0);
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
}

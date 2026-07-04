use eframe;
use egui::epaint::{PathShape, PathStroke};
use egui::{Align2, Color32, FontId, Key, Painter, Pos2, Rect, Sense, Stroke, Vec2};
use slotmap::{new_key_type, SlotMap};
use std::collections::HashMap;

use crate::gui::geometry::{snap_to_grid, tunnel_shape, GRID_SIZE};
use crate::gui::placed_component::{ComponentDef, PlacedComponent};
use crate::gui::shape::{tessellate_path, ComponentShape, BUBBLE_R};
use crate::gui::theme::Theme;
use crate::io::{
    CircuitFile, ComponentEntry, LoadError, TunnelEntry, TunnelWireEntry, WireEntry,
    CURRENT_VERSION,
};
use crate::sim::circuit::{Circuit, TunnelKey, TunnelRole};
use crate::sim::component::{
    Adder, CompKey, Demux, Encoder, FanDirection, Gate, GateOp, InIdx, Input, Mux, OutIdx, PinId,
    Reg, Subtractor,
};
use crate::sim::value::Value;

// ── Constants ─────────────────────────────────────────────────────────────────

const PIN_RADIUS: f32 = 3.0;
const WIRE_THICKNESS_THIN: f32 = 2.0;
const WIRE_THICKNESS_THICK: f32 = 4.0;
const LABEL_FONT_SIZE: f32 = 8.0;
const COMP_STROKE: f32 = 1.5;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SHA: &str = env!("OSMILOG_GIT_SHA");

// ── PlacedTunnel ──────────────────────────────────────────────────────────────

// Visual record for a Tunnel (net label / off-page connector). Deliberately
// not a PlacedComponent/ComponentDef variant — Tunnel lives at the Circuit
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
    pub grid_pos: [i32; 2],
}

// ── Wire ──────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Wire {
    pub src_comp: CompKey,
    pub src_pin: OutIdx,
    pub dst_comp: CompKey,
    pub dst_pin: InIdx,
}

// Records that a component pin is tied to a Tunnel. Unlike Wire, resolves
// its net live via Component::net_of rather than caching a NetKey, since a
// cached NetKey can go stale across a merge() (see draw loop for details).
#[derive(Clone, Debug)]
pub struct TunnelWire {
    pub tunnel: TunnelKey,
    pub comp: CompKey,
    pub pin: PinId,
}

// ── Selected / DragSource ─────────────────────────────────────────────────────

// A component and a tunnel are both selectable/draggable canvas entities;
// using one enum (rather than parallel Option<CompKey>/Option<TunnelKey>
// fields) avoids a "can both be Some, who wins" desync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Component(PlacedCompKey),
    Tunnel(PlacedTunnelKey),
}

// The origin of an in-progress wire drag: either a component's output pin
// or a Feed tunnel's pin (Feed tunnels behave like Input for dragging
// purposes - valid drag-start only; Pull tunnels behave like Output -
// valid drop-target only, checked separately at drag_stopped()).
#[derive(Debug, Clone, Copy)]
pub enum DragSource {
    Component(PlacedCompKey, OutIdx),
    Tunnel(PlacedTunnelKey),
}

// ── InteractionMode ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum InteractionMode {
    Idle,
    Placing {
        def: ComponentDef,
    },
    PlacingTunnel {
        role: TunnelRole,
    },
    WireDrag {
        src: DragSource,
        current_end: Pos2,
    },
    ComponentDrag {
        key: Selected,
        drag_origin: Pos2,
        original_grid_pos: [i32; 2],
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
    pub components: SlotMap<PlacedCompKey, PlacedComponent>,
    pub tunnels: SlotMap<PlacedTunnelKey, PlacedTunnel>,
    pub wires: Vec<Wire>,
    pub tunnel_wires: Vec<TunnelWire>,
    pub mode: InteractionMode,
    pub pan: Vec2,
    pub selected: Option<Selected>,
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
            components: SlotMap::default(),
            tunnels: SlotMap::default(),
            wires: Vec::new(),
            tunnel_wires: Vec::new(),
            mode: InteractionMode::Idle,
            pan: Vec2::ZERO,
            selected: None,
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

    fn place_component(&mut self, def: ComponentDef, grid_pos: [i32; 2]) -> PlacedCompKey {
        let comp = def.make_component();
        let key = self.circuit.add_component(comp);
        let pc = PlacedComponent { key, def, grid_pos };
        self.components.insert(pc)
    }

    fn place_tunnel(&mut self, role: TunnelRole, grid_pos: [i32; 2]) -> PlacedTunnelKey {
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
        grid_pos: [i32; 2],
    ) -> PlacedTunnelKey {
        let key = self.circuit.add_tunnel(label.clone(), role);
        let pt = PlacedTunnel {
            key,
            label,
            role,
            grid_pos,
        };
        self.tunnels.insert(pt)
    }

    // ── Save / load ──────────────────────────────────────────────────────

    pub fn to_circuit_file(&self) -> CircuitFile {
        // CompKey/TunnelKey (the sim's own keys, held on PlacedComponent/
        // PlacedTunnel) -> position in the Vec being emitted, so wires can
        // be re-expressed as indices instead of slotmap keys. Populated as
        // a side effect of building `components`/`tunnels` below, so it
        // must be fully built before `wires`/`tunnel_wires` read it.
        let mut comp_index: HashMap<CompKey, usize> = HashMap::new();
        let components: Vec<ComponentEntry> = self
            .components
            .values()
            .enumerate()
            .map(|(i, pc)| {
                comp_index.insert(pc.key, i);
                ComponentEntry {
                    def: pc.def.clone(),
                    grid_pos: pc.grid_pos,
                }
            })
            .collect();

        let mut tunnel_index: HashMap<TunnelKey, usize> = HashMap::new();
        let tunnels: Vec<TunnelEntry> = self
            .tunnels
            .values()
            .enumerate()
            .map(|(i, pt)| {
                tunnel_index.insert(pt.key, i);
                TunnelEntry {
                    label: pt.label.clone(),
                    role: pt.role,
                    grid_pos: pt.grid_pos,
                }
            })
            .collect();

        let wires = self
            .wires
            .iter()
            .map(|w| WireEntry {
                src: comp_index[&w.src_comp],
                src_pin: w.src_pin.0,
                dst: comp_index[&w.dst_comp],
                dst_pin: w.dst_pin.0,
            })
            .collect();

        let tunnel_wires = self
            .tunnel_wires
            .iter()
            .map(|tw| {
                let (is_input, pin_index) = match tw.pin {
                    PinId::In(InIdx(i)) => (true, i),
                    PinId::Out(OutIdx(i)) => (false, i),
                };
                TunnelWireEntry {
                    tunnel: tunnel_index[&tw.tunnel],
                    comp: comp_index[&tw.comp],
                    is_input,
                    pin_index,
                }
            })
            .collect();

        CircuitFile {
            version: CURRENT_VERSION,
            components,
            tunnels,
            wires,
            tunnel_wires,
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
        self.wires = Vec::new();
        self.tunnel_wires = Vec::new();
        self.selected = None;
        self.mode = InteractionMode::Idle;
        self.last_settle_error = None;

        let comp_keys: Vec<CompKey> = file
            .components
            .iter()
            .map(|entry| {
                let key = self.place_component(entry.def.clone(), entry.grid_pos);
                self.components[key].key
            })
            .collect();

        let tunnel_keys: Vec<TunnelKey> = file
            .tunnels
            .iter()
            .map(|entry| {
                let key =
                    self.place_tunnel_labeled(entry.label.clone(), entry.role, entry.grid_pos);
                self.tunnels[key].key
            })
            .collect();

        for w in &file.wires {
            let src = comp_keys[w.src];
            let dst = comp_keys[w.dst];
            self.circuit
                .link(src, PinId::output(w.src_pin), dst, PinId::input(w.dst_pin));
            self.wires.push(Wire {
                src_comp: src,
                src_pin: OutIdx(w.src_pin),
                dst_comp: dst,
                dst_pin: InIdx(w.dst_pin),
            });
        }

        for tw in &file.tunnel_wires {
            let tunnel = tunnel_keys[tw.tunnel];
            let comp = comp_keys[tw.comp];
            let pin = if tw.is_input {
                PinId::input(tw.pin_index)
            } else {
                PinId::output(tw.pin_index)
            };
            self.circuit.link_tunnel(tunnel, comp, pin);
            self.tunnel_wires.push(TunnelWire { tunnel, comp, pin });
        }

        let result = self.circuit.settle();
        self.record_settle_result(result);
        Ok(())
    }

    /// Shows property menu for the currently selected item. ComponentDef for the UI element is
    /// cloned. If the user edits a property, call `self.reconfigure_component()` with an updated ComponentDef
    fn show_properties(&mut self, ui: &mut egui::Ui) {
        let Some(sel) = self.selected else {
            ui.label("Click a component or tunnel to select it.");
            return;
        };
        match sel {
            Selected::Component(key) => self.show_component_properties(key, ui),
            Selected::Tunnel(key) => self.show_tunnel_properties(key, ui),
        }

        ui.separator();
        if ui.button("Delete").clicked() {
            match sel {
                Selected::Component(key) => self.delete_component(key),
                Selected::Tunnel(key) => self.delete_tunnel(key),
            }
        }
    }

    fn show_tunnel_properties(&mut self, key: PlacedTunnelKey, ui: &mut egui::Ui) {
        let pt = &mut self.tunnels[key];

        ui.heading(match pt.role {
            TunnelRole::Feed => "TUNNEL (FEED)",
            TunnelRole::Pull => "TUNNEL (PULL)",
        });
        ui.separator();
        ui.label("Label:");
        let mut label = pt.label.clone();
        let response = ui.text_edit_singleline(&mut label);
        if response.changed() {
            pt.label = label.clone();
        }

        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            self.circuit.rename_tunnel(pt.key, label.clone());
            pt.label = label;
            let result = self.circuit.settle();
            self.record_settle_result(result);
        }
    }

    // TODO: can't this be an index for a PlacedComponent instead?
    fn show_component_properties(&mut self, key: PlacedCompKey, ui: &mut egui::Ui) {
        let pc = &self.components[key];
        let comp_key = pc.key;

        ui.heading(pc.def.label());
        ui.separator();

        let def = pc.def.clone();
        match def {
            ComponentDef::Input(Input {
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
                    self.reconfigure_component(key, ComponentDef::Input(Input { bits, width }));
                }
            }
            ComponentDef::Output => {
                let val = self.circuit.read_output(comp_key);
                let val_str = match val {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                    Value::Invalid => "Invalid (width mismatch)".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
            ComponentDef::Gate(Gate {
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
                        ComponentDef::Gate(Gate {
                            op,
                            n_inputs,
                            width,
                        }),
                    );
                }
            }
            ComponentDef::Mux(Mux {
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
                        ComponentDef::Mux(Mux {
                            data_width,
                            sel_width,
                        }),
                    );
                }
            }
            ComponentDef::Demux(Demux {
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
                        ComponentDef::Demux(Demux {
                            data_width,
                            sel_width,
                        }),
                    );
                }
            }
            ComponentDef::Reg(Reg { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentDef::Reg(Reg { data_width }));
                }

                let cur = self.circuit.components[comp_key].pins.out_cache[0];
                let val_str = match cur {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                    Value::Invalid => "Invalid (width mismatch)".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
            ComponentDef::Encoder(Encoder { mut sel_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Sel width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut sel_width).range(0..=4))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentDef::Encoder(Encoder { sel_width }));
                }
            }
            ComponentDef::Adder(Adder { mut data_width }) => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentDef::Adder(Adder { data_width }));
                }
            }
            ComponentDef::Subtractor(Subtractor { mut data_width }) => {
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
                        ComponentDef::Subtractor(Subtractor { data_width }),
                    );
                }
            }
            ComponentDef::Splitter {
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
                        ComponentDef::Splitter {
                            width,
                            arm_bits,
                            direction,
                        },
                    );
                }
            }
        }
    }

    // TODO: write docs, return result
    fn reconfigure_component(&mut self, old_pc_key: PlacedCompKey, new_def: ComponentDef) {
        let old_pc = &self.components[old_pc_key];
        let old_key = old_pc.key;
        let grid_pos = old_pc.grid_pos;

        let new_comp = new_def.make_component();
        let new_n_in = new_comp.pins.inputs.len();
        let new_n_out = new_comp.pins.outputs.len();

        // Wires touching old_key whose pin index is still valid for the new component
        let surviving: Vec<Wire> = self
            .wires
            .iter()
            .filter(|w| {
                if w.src_comp == old_key {
                    (w.src_pin.0 as usize) < new_n_out
                } else if w.dst_comp == old_key {
                    (w.dst_pin.0 as usize) < new_n_in
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        // Tunnel wires touching old_key whose pin index is still valid for
        // the new component - same "survives if the pin still exists" rule
        // as regular wires above, so a tunnel connection isn't silently
        // dropped just because e.g. an Input's value got toggled.
        let surviving_tunnel_wires: Vec<TunnelWire> = self
            .tunnel_wires
            .iter()
            .filter(|tw| {
                tw.comp == old_key
                    && match tw.pin {
                        PinId::In(i) => (i.0 as usize) < new_n_in,
                        PinId::Out(i) => (i.0 as usize) < new_n_out,
                    }
            })
            .cloned()
            .collect();

        self.circuit.remove_component(old_key);
        self.wires
            .retain(|w| w.src_comp != old_key && w.dst_comp != old_key);
        self.tunnel_wires.retain(|tw| tw.comp != old_key);

        let new_key = self.circuit.add_component(new_comp);

        self.components[old_pc_key] = PlacedComponent {
            key: new_key,
            def: new_def,
            grid_pos,
        };

        for w in surviving {
            let (src, src_pin, dst, dst_pin) = if w.src_comp == old_key {
                (new_key, w.src_pin, w.dst_comp, w.dst_pin)
            } else {
                (w.src_comp, w.src_pin, new_key, w.dst_pin)
            };
            self.circuit
                .link(src, PinId::output(src_pin.0), dst, PinId::input(dst_pin.0));
            self.wires.push(Wire {
                src_comp: src,
                src_pin,
                dst_comp: dst,
                dst_pin,
            });
        }

        for tw in surviving_tunnel_wires {
            self.circuit.link_tunnel(tw.tunnel, new_key, tw.pin);
            self.tunnel_wires.push(TunnelWire {
                tunnel: tw.tunnel,
                comp: new_key,
                pin: tw.pin,
            });
        }

        let result = self.circuit.settle();
        self.record_settle_result(result);
        // Now that Selected holds a PlacedCompKey, it doesn't need to be updated on reconfigure
        self.selected = Some(Selected::Component(old_pc_key));
    }

    // Removes a placed component from both the circuit and the GUI's visual
    // records. circuit.remove_component unlinks its nets and re-evaluates
    // affected sinks; here we drop the visual Wire/TunnelWire records that
    // referenced it (same "touches this CompKey" retain as reconfigure_component)
    // so the draw loop doesn't chase a dangling key.
    fn delete_component(&mut self, key: PlacedCompKey) {
        let comp_key = self.components[key].key;
        self.circuit.remove_component(comp_key);
        self.wires
            .retain(|w| w.src_comp != comp_key && w.dst_comp != comp_key);
        self.tunnel_wires.retain(|tw| tw.comp != comp_key);
        self.components.remove(key);
        if self.selected == Some(Selected::Component(key)) {
            self.selected = None;
        }
        let result = self.circuit.settle();
        self.record_settle_result(result);
    }

    // Removes a placed tunnel from both the circuit and the GUI's visual
    // records. circuit.remove_tunnel detaches its net and re-dirties the label
    // group's feed nets; here we drop the visual TunnelWire records that tied a
    // component pin to it.
    fn delete_tunnel(&mut self, key: PlacedTunnelKey) {
        let tunnel_key = self.tunnels[key].key;
        self.circuit.remove_tunnel(tunnel_key);
        self.tunnel_wires.retain(|tw| tw.tunnel != tunnel_key);
        self.tunnels.remove(key);
        if self.selected == Some(Selected::Tunnel(key)) {
            self.selected = None;
        }
        let result = self.circuit.settle();
        self.record_settle_result(result);
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
                                    def: ComponentDef::Gate(Gate {
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
                            def: ComponentDef::Input(Input { bits: 0, width: 1 }),
                        };
                        ui.close();
                    }
                    if ui.button("Output").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Output,
                        };
                        ui.close();
                    }
                    if ui.button("Mux").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Mux(Mux {
                                data_width: 1,
                                sel_width: 1,
                            }),
                        };
                        ui.close();
                    }
                    if ui.button("Demux").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Demux(Demux {
                                data_width: 1,
                                sel_width: 1,
                            }),
                        };
                        ui.close();
                    }
                    if ui.button("Splitter").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Splitter {
                                width: 2,
                                arm_bits: vec![vec![0], vec![1]],
                                direction: FanDirection::Right,
                            },
                        };
                        ui.close();
                    }
                    if ui.button("Encoder").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Encoder(Encoder { sel_width: 1 }),
                        };
                        ui.close();
                    }
                    if ui.button("Adder").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Adder(Adder { data_width: 1 }),
                        };
                        ui.close();
                    }
                    if ui.button("Subtractor").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Subtractor(Subtractor { data_width: 1 }),
                        };
                        ui.close();
                    }
                    ui.menu_button("Memory", |ui| {
                        if ui.button("Register").clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentDef::Reg(Reg { data_width: 1 }),
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
                    let result = self.circuit.tick_clock();
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
                if let InteractionMode::ComponentDrag {
                    key,
                    original_grid_pos,
                    ..
                } = self.mode
                {
                    match key {
                        Selected::Component(k) => self.components[k].grid_pos = original_grid_pos,
                        Selected::Tunnel(k) => self.tunnels[k].grid_pos = original_grid_pos,
                    }
                }
                self.mode = InteractionMode::Idle;
            }

            // Backspace deletes the current selection. Guard on widget focus so
            // a Backspace aimed at the tunnel-label text field (or any focused
            // widget) edits text instead of deleting.
            let editing_text = ctx.memory(|m| m.focused().is_some());
            if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) && !editing_text {
                if let Some(sel) = self.selected {
                    match sel {
                        Selected::Component(k) => self.delete_component(k),
                        Selected::Tunnel(k) => self.delete_tunnel(k),
                    }
                }
            }

            painter.rect_filled(clip_rect, 0.0, theme.canvas_bg);
            draw_grid(&painter, clip_rect, pan, theme);

            // Draw wires. Resolves the net live via Component::net_of rather
            // than caching a NetKey on Wire: a cached key can go stale if a
            // later link() merges its net away (merge() always removes one
            // of the two merged NetKeys), which would otherwise panic here
            // the very next frame.
            for wire in &self.wires {
                let (p0, p1) = {
                    // FIXME: Wire should probably hold a PlacedCompKey
                    let src = self
                        .components
                        .values()
                        .find(|pc| pc.key == wire.src_comp)
                        .unwrap();
                    let dst = self
                        .components
                        .values()
                        .find(|pc| pc.key == wire.dst_comp)
                        .unwrap();
                    (
                        comp_pin_pos(
                            &src.def.shape(),
                            src.grid_pos,
                            pan,
                            PinId::output(wire.src_pin.0),
                        ),
                        comp_pin_pos(
                            &dst.def.shape(),
                            dst.grid_pos,
                            pan,
                            PinId::input(wire.dst_pin.0),
                        ),
                    )
                };
                let net =
                    self.circuit.components[wire.src_comp].net_of(PinId::output(wire.src_pin.0));
                let color = net
                    .map(|nk| value_stroke(theme, self.circuit.nets[nk].value))
                    .unwrap_or(value_stroke(theme, Value::Floating));
                draw_wire(&painter, p0, p1, color);
            }

            // Draw tunnel wires (component pin <-> tunnel), same live-lookup
            // approach as above - no cached NetKey.
            for tw in &self.tunnel_wires {
                let (Some(pt), Some(pc)) = (
                    // FIXME: TunnelWire should probably hold a PlacedTunnelKey
                    self.tunnels.values().find(|pt| pt.key == tw.tunnel),
                    self.components.values().find(|pc| pc.key == tw.comp),
                ) else {
                    continue;
                };
                let p0 = tunnel_pin_pos(pt, pan);
                let p1 = comp_pin_pos(&pc.def.shape(), pc.grid_pos, pan, tw.pin);
                let net = self.circuit.components[tw.comp].net_of(tw.pin);
                let color = net
                    .map(|nk| value_stroke(theme, self.circuit.nets[nk].value))
                    .unwrap_or(value_stroke(theme, Value::Floating));
                draw_wire(&painter, p0, p1, color);
            }

            // Draw components
            for (pc_key, pc) in &self.components {
                let is_selected = self.selected == Some(Selected::Component(pc_key));
                draw_component(&painter, pc, pan, &self.circuit, is_selected, theme);
            }

            // Draw tunnels
            for (pt_key, pt) in &self.tunnels {
                let is_selected = self.selected == Some(Selected::Tunnel(pt_key));
                draw_tunnel(&painter, pt, pan, &self.circuit, is_selected, theme);
            }

            let pointer = response
                .interact_pointer_pos()
                .or_else(|| ctx.pointer_hover_pos());

            // Mode-specific interaction
            let mode = self.mode.clone();
            match mode {
                InteractionMode::Idle => {
                    if response.drag_started() {
                        let origin = ctx.input(|i| i.pointer.press_origin());
                        if let Some(pos) = origin {
                            if let Some((comp_key, PinId::Out(out_idx))) =
                                pin_at_pos(self.components.iter(), pan, pos, PinKind::Output)
                            {
                                // Output-pin drag → start wire (highest priority)
                                self.mode = InteractionMode::WireDrag {
                                    src: DragSource::Component(comp_key, out_idx),
                                    current_end: pos,
                                };
                            } else if let Some(tunnel_key) =
                                tunnel_pin_at_pos(self.tunnels.iter(), pan, pos)
                            {
                                // Feed-tunnel pin drag → start wire
                                self.mode = InteractionMode::WireDrag {
                                    src: DragSource::Tunnel(tunnel_key),
                                    current_end: pos,
                                };
                            } else if let Some(sel) = self.selected {
                                // Selected component/tunnel body drag → move it
                                let (rect, grid_pos) = match sel {
                                    Selected::Component(key) => {
                                        let pc = &self.components[key];
                                        (component_bounding_rect(pc, pan), pc.grid_pos)
                                    }
                                    Selected::Tunnel(key) => {
                                        let pt = &self.tunnels[key];
                                        (tunnel_bounding_rect(pt, pan), pt.grid_pos)
                                    }
                                };
                                if rect.contains(pos) {
                                    self.mode = InteractionMode::ComponentDrag {
                                        key: sel,
                                        drag_origin: pos,
                                        original_grid_pos: grid_pos,
                                    };
                                }
                            }
                        }
                    }

                    // Click any component/tunnel body to select it (components
                    // take priority over tunnels on overlap); click empty
                    // canvas to deselect.
                    if response.clicked() {
                        if let Some(pos) = pointer {
                            let maybe_comp = self
                                .components
                                .iter()
                                .find(|(_key, pc)| component_bounding_rect(pc, pan).contains(pos))
                                .map(|(key, _pc)| Selected::Component(key));
                            let maybe_tunnel = self
                                .tunnels
                                .iter()
                                .find(|(_key, pt)| tunnel_bounding_rect(pt, pan).contains(pos))
                                .map(|(key, _pt)| Selected::Tunnel(key));
                            self.selected = maybe_comp.or(maybe_tunnel);
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

                InteractionMode::WireDrag { src, current_end } => {
                    let p0 = match src {
                        DragSource::Component(pc_key, out_idx) => {
                            let src_pc = &self.components[pc_key];
                            comp_pin_pos(
                                &src_pc.def.shape(),
                                src_pc.grid_pos,
                                pan,
                                PinId::output(out_idx.0),
                            )
                        }
                        DragSource::Tunnel(pt_key) => {
                            let src_pt = &self.tunnels[pt_key];
                            tunnel_pin_pos(src_pt, pan)
                        }
                    };

                    let end = pointer.unwrap_or(current_end);
                    let stroke = Stroke::new(WIRE_THICKNESS_THIN, theme.wire_drag_preview);
                    draw_wire(&painter, p0, end, stroke);

                    self.mode = InteractionMode::WireDrag {
                        src,
                        current_end: end,
                    };

                    if response.drag_stopped() {
                        let target = pin_at_pos(self.components.iter(), pan, end, PinKind::Input);
                        match (src, target) {
                            (
                                DragSource::Component(src_pc, src_pin),
                                Some((dst_pc, PinId::In(in_idx))),
                            ) => {
                                let src_comp = self.components[src_pc].key;
                                let dst_comp = self.components[dst_pc].key;
                                if dst_comp != src_comp {
                                    self.circuit.link(
                                        src_comp,
                                        PinId::output(src_pin.0),
                                        dst_comp,
                                        PinId::input(in_idx.0),
                                    );
                                    let result = self.circuit.settle();
                                    self.record_settle_result(result);
                                    self.wires.push(Wire {
                                        src_comp,
                                        src_pin,
                                        dst_comp,
                                        dst_pin: in_idx,
                                    });
                                }
                            }
                            (DragSource::Component(src_pc_key, src_pin), None) => {
                                // Didn't land on a component input pin; check
                                // whether it landed on a Pull tunnel instead.
                                let tunnel_key = self
                                    .tunnels
                                    .values()
                                    .find(|pt| {
                                        pt.role == TunnelRole::Pull
                                            && tunnel_bounding_rect(pt, pan).contains(end)
                                    })
                                    .map(|pt| pt.key);
                                if let Some(tunnel_key) = tunnel_key {
                                    let src_comp = self.components[src_pc_key].key;
                                    let pin = PinId::output(src_pin.0);

                                    self.circuit.link_tunnel(tunnel_key, src_comp, pin);
                                    let result = self.circuit.settle();
                                    self.record_settle_result(result);
                                    self.tunnel_wires.push(TunnelWire {
                                        tunnel: tunnel_key,
                                        comp: src_comp,
                                        pin,
                                    });
                                }
                            }
                            (DragSource::Tunnel(src_pt), Some((dst_pc, PinId::In(in_idx)))) => {
                                let tunnel_key = self.tunnels[src_pt].key;
                                let dst_comp = self.components[dst_pc].key;
                                let pin = PinId::input(in_idx.0);

                                self.circuit.link_tunnel(tunnel_key, dst_comp, pin);
                                let result = self.circuit.settle();
                                self.record_settle_result(result);
                                self.tunnel_wires.push(TunnelWire {
                                    tunnel: tunnel_key,
                                    comp: dst_comp,
                                    pin,
                                });
                            }
                            _ => {}
                        }
                        self.mode = InteractionMode::Idle;
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
                        let new_grid_pos = [
                            original_grid_pos[0] + delta_x,
                            original_grid_pos[1] + delta_y,
                        ];
                        match key {
                            Selected::Component(k) => self.components[k].grid_pos = new_grid_pos,
                            Selected::Tunnel(k) => self.tunnels[k].grid_pos = new_grid_pos,
                        }
                    }
                    if response.drag_stopped() {
                        self.mode = InteractionMode::Idle;
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
        pc.grid_pos[0] as f32 * GRID_SIZE + pan.x,
        pc.grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    Rect::from_min_size(tl, size)
}

// Takes an already-computed ComponentShape rather than a &PlacedComponent so
// callers that need multiple pins from the same component (draw_component,
// pin_at_pos) compute shape() once and reuse it, instead of each call
// redundantly rebuilding the whole shape (outline/anchors/bubbles Vecs)
// just to read one anchor.
fn comp_pin_pos(shape: &ComponentShape, grid_pos: [i32; 2], pan: Vec2, pin: PinId) -> Pos2 {
    let tl = egui::pos2(
        grid_pos[0] as f32 * GRID_SIZE + pan.x,
        grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, shape.size);
    let anchor = match pin {
        PinId::In(InIdx(i)) => &shape.input_anchors[i as usize],
        PinId::Out(OutIdx(i)) => &shape.output_anchors[i as usize],
    };
    egui::pos2(
        rect.left() + anchor.norm_pos.x * rect.width(),
        rect.top() + anchor.norm_pos.y * rect.height(),
    ) + anchor.wire_dir * anchor.pixel_offset
}

fn tunnel_bounding_rect(pt: &PlacedTunnel, pan: Vec2) -> Rect {
    let size = tunnel_shape(pt.role).size;
    let tl = egui::pos2(
        pt.grid_pos[0] as f32 * GRID_SIZE + pan.x,
        pt.grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    Rect::from_min_size(tl, size)
}

fn tunnel_pin_pos(pt: &PlacedTunnel, pan: Vec2) -> Pos2 {
    let shape = tunnel_shape(pt.role);
    let tl = egui::pos2(
        pt.grid_pos[0] as f32 * GRID_SIZE + pan.x,
        pt.grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, shape.size);
    let anchor = match pt.role {
        TunnelRole::Feed => &shape.output_anchors[0],
        TunnelRole::Pull => &shape.input_anchors[0],
    };
    egui::pos2(
        rect.left() + anchor.norm_pos.x * rect.width(),
        rect.top() + anchor.norm_pos.y * rect.height(),
    ) + anchor.wire_dir * anchor.pixel_offset
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
fn tunnel_pin_at_pos<'a>(
    tunnels: impl Iterator<Item = (PlacedTunnelKey, &'a PlacedTunnel)>,
    pan: Vec2,
    pos: Pos2,
) -> Option<PlacedTunnelKey> {
    let hit_r = PIN_RADIUS * 2.0;
    for (key, tunnel) in tunnels {
        if tunnel.role == TunnelRole::Feed && tunnel_pin_pos(tunnel, pan).distance(pos) <= hit_r {
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

fn draw_wire(painter: &Painter, p0: Pos2, p1: Pos2, stroke: Stroke) {
    let mid_x = (p0.x + p1.x) / 2.0;
    let elbow1 = egui::pos2(mid_x, p0.y);
    let elbow2 = egui::pos2(mid_x, p1.y);
    // A single open Path (rather than separate line_segment calls) lets the tessellator
    // join the elbow corners, avoiding the notch that butt-capped independent segments leave.
    painter.add(PathShape::line(vec![p0, elbow1, elbow2, p1], stroke));
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
            let boundary = egui::pos2(
                rect.left() + anchor.norm_pos.x * rect.width(),
                rect.top() + anchor.norm_pos.y * rect.height(),
            );
            let center = boundary + anchor.wire_dir * BUBBLE_R;
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
            FontId::monospace(LABEL_FONT_SIZE),
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

fn draw_ghost(painter: &Painter, def: &ComponentDef, grid_pos: [i32; 2], pan: Vec2, theme: Theme) {
    let shape = def.shape();
    let tl = egui::pos2(
        grid_pos[0] as f32 * GRID_SIZE + pan.x,
        grid_pos[1] as f32 * GRID_SIZE + pan.y,
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
    grid_pos: [i32; 2],
    pan: Vec2,
    theme: Theme,
) {
    let shape = tunnel_shape(role);
    let tl = egui::pos2(
        grid_pos[0] as f32 * GRID_SIZE + pan.x,
        grid_pos[1] as f32 * GRID_SIZE + pan.y,
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
    use crate::sim::component::GateOp;

    fn place(app: &mut OsmilogApp, def: ComponentDef) -> PlacedCompKey {
        app.place_component(def, [0, 0])
    }

    #[test]
    fn test_circuit_file_round_trip_basic() {
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentDef::Input(Input { bits: 1, width: 1 }));
        let b = place(&mut app, ComponentDef::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut app,
            ComponentDef::Gate(Gate {
                op: GateOp::And,
                n_inputs: 2,
                width: 1,
            }),
        );
        let o = place(&mut app, ComponentDef::Output);

        let (a_key, b_key, g_key, o_key) = (
            app.components[a].key,
            app.components[b].key,
            app.components[g].key,
            app.components[o].key,
        );
        app.circuit
            .link(a_key, PinId::output(0), g_key, PinId::input(0));
        app.circuit
            .link(b_key, PinId::output(0), g_key, PinId::input(1));
        app.circuit
            .link(g_key, PinId::output(0), o_key, PinId::input(0));
        app.wires.push(Wire {
            src_comp: a_key,
            src_pin: OutIdx(0),
            dst_comp: g_key,
            dst_pin: InIdx(0),
        });
        app.wires.push(Wire {
            src_comp: b_key,
            src_pin: OutIdx(0),
            dst_comp: g_key,
            dst_pin: InIdx(1),
        });
        app.wires.push(Wire {
            src_comp: g_key,
            src_pin: OutIdx(0),
            dst_comp: o_key,
            dst_pin: InIdx(0),
        });

        app.circuit.settle().unwrap();
        assert_eq!(app.circuit.read_output(o_key), Value::new(1, 1));

        // Save -> JSON -> parse -> load into a fresh app, and confirm the
        // loaded circuit behaves identically.
        let file = app.to_circuit_file();
        let json = file.to_json().unwrap();
        let file2 = CircuitFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_circuit_file(&file2).unwrap();

        assert_eq!(loaded.components.len(), 4);
        assert_eq!(loaded.wires.len(), 3);
        let loaded_out_key = loaded
            .components
            .values()
            .find(|pc| matches!(pc.def, ComponentDef::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.circuit.read_output(loaded_out_key), Value::new(1, 1));
    }

    #[test]
    fn test_circuit_file_round_trip_with_tunnel() {
        let mut app = OsmilogApp::empty();
        let inp = place(&mut app, ComponentDef::Input(Input { bits: 1, width: 1 }));
        let out = place(&mut app, ComponentDef::Output);
        let feed = app.place_tunnel(TunnelRole::Feed, [0, 0]);
        let pull = app.place_tunnel(TunnelRole::Pull, [1, 1]);

        // Tunnels connect to each other by matching label, not by wire -
        // give `pull` the same label as `feed` so they form one virtual net.
        let shared_label = app.tunnels[feed].label.clone();
        app.circuit
            .rename_tunnel(app.tunnels[pull].key, shared_label.clone());
        app.tunnels[pull].label = shared_label;

        let (inp_key, out_key, feed_key, pull_key) = (
            app.components[inp].key,
            app.components[out].key,
            app.tunnels[feed].key,
            app.tunnels[pull].key,
        );
        // Pull reads FROM its attached net (here: inp's output) and
        // contributes that value TO the shared label group; Feed drives its
        // attached net (here: out's input) FROM the group's resolved value.
        app.circuit.link_tunnel(pull_key, inp_key, PinId::output(0));
        app.tunnel_wires.push(TunnelWire {
            tunnel: pull_key,
            comp: inp_key,
            pin: PinId::output(0),
        });
        app.circuit.link_tunnel(feed_key, out_key, PinId::input(0));
        app.tunnel_wires.push(TunnelWire {
            tunnel: feed_key,
            comp: out_key,
            pin: PinId::input(0),
        });

        app.circuit.settle().unwrap();
        assert_eq!(app.circuit.read_output(out_key), Value::new(1, 1));

        let file = app.to_circuit_file();
        let json = file.to_json().unwrap();
        let file2 = CircuitFile::from_json(&json).unwrap();

        let mut loaded = OsmilogApp::empty();
        loaded.load_circuit_file(&file2).unwrap();

        assert_eq!(loaded.tunnels.len(), 2);
        let loaded_out_key = loaded
            .components
            .values()
            .find(|pc| matches!(pc.def, ComponentDef::Output))
            .unwrap()
            .key;
        assert_eq!(loaded.circuit.read_output(loaded_out_key), Value::new(1, 1));
    }

    #[test]
    fn test_load_circuit_file_rejects_bad_component_index() {
        let file = CircuitFile {
            version: CURRENT_VERSION,
            components: vec![ComponentEntry {
                def: ComponentDef::Output,
                grid_pos: [0, 0],
            }],
            tunnels: vec![],
            wires: vec![WireEntry {
                src: 5,
                src_pin: 0,
                dst: 0,
                dst_pin: 0,
            }],
            tunnel_wires: vec![],
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
            wires: vec![],
            tunnel_wires: vec![],
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
    fn test_delete_component_drops_visual_records_and_refreshes_downstream() {
        // Input -> NOT(g) -> Output, then delete the middle gate: its visual
        // record and both wires touching it must be gone, the circuit component
        // removed, the selection cleared, and the downstream Output refreshed
        // (its input is now Floating).
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentDef::Input(Input { bits: 1, width: 1 }));
        let g = place(
            &mut app,
            ComponentDef::Gate(Gate {
                op: GateOp::Not,
                n_inputs: 1,
                width: 1,
            }),
        );
        let o = place(&mut app, ComponentDef::Output);
        let (a_key, g_key, o_key) = (
            app.components[a].key,
            app.components[g].key,
            app.components[o].key,
        );
        app.circuit
            .link(a_key, PinId::output(0), g_key, PinId::input(0));
        app.circuit
            .link(g_key, PinId::output(0), o_key, PinId::input(0));
        app.wires.push(Wire {
            src_comp: a_key,
            src_pin: OutIdx(0),
            dst_comp: g_key,
            dst_pin: InIdx(0),
        });
        app.wires.push(Wire {
            src_comp: g_key,
            src_pin: OutIdx(0),
            dst_comp: o_key,
            dst_pin: InIdx(0),
        });
        app.circuit.settle().unwrap();
        assert_eq!(app.circuit.read_output(o_key), Value::new(0, 1)); // NOT(1) = 0
        app.selected = Some(Selected::Component(g));

        app.delete_component(g);

        assert!(!app.components.contains_key(g));
        assert!(app.circuit.components.get(g_key).is_none());
        // Only the a->? wire is gone along with g->o; no wire references g_key.
        assert!(app
            .wires
            .iter()
            .all(|w| w.src_comp != g_key && w.dst_comp != g_key));
        assert_eq!(app.selected, None);
        // Output's input pin was nulled and re-evaluated to Floating.
        assert_eq!(app.circuit.read_output(o_key), Value::Floating);
    }

    #[test]
    fn test_delete_tunnel_drops_visual_records() {
        // A component pin tied to a tunnel: deleting the tunnel removes its
        // visual record, drops the TunnelWire referencing it, and clears the
        // selection.
        let mut app = OsmilogApp::empty();
        let a = place(&mut app, ComponentDef::Input(Input { bits: 1, width: 1 }));
        let t = app.place_tunnel(TunnelRole::Pull, [1, 1]);
        let (a_key, t_key) = (app.components[a].key, app.tunnels[t].key);
        let pin = PinId::output(0);
        app.circuit.link_tunnel(t_key, a_key, pin);
        app.tunnel_wires.push(TunnelWire {
            tunnel: t_key,
            comp: a_key,
            pin,
        });
        app.circuit.settle().unwrap();
        app.selected = Some(Selected::Tunnel(t));

        app.delete_tunnel(t);

        assert!(!app.tunnels.contains_key(t));
        assert!(app.circuit.tunnels.get(t_key).is_none());
        assert!(app.tunnel_wires.iter().all(|tw| tw.tunnel != t_key));
        assert_eq!(app.selected, None);
    }
}

use eframe;
use egui::epaint::{PathShape, PathStroke};
use egui::{Align2, Color32, FontId, Key, Painter, Pos2, Rect, Sense, Stroke, Vec2};

use crate::{
    circuit::{Circuit, TunnelKey, TunnelRole},
    component::{CompKey, Component, GateOp, InIdx, OutIdx, PinId},
    geometry::{
        demux_shape, demux_size, gate_shape, gate_size, mux_shape, mux_size, rect_outline,
        reg_shape, reg_size, snap_to_grid, tunnel_shape, COMP_MIN_HEIGHT, COMP_WIDTH, GRID_SIZE,
    },
    shape::{tessellate_path, ComponentShape, PinAnchor, BUBBLE_R},
    value::Value,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const PIN_RADIUS: f32 = 3.0;
const WIRE_THICKNESS_THIN: f32 = 2.0;
const WIRE_THICKNESS_THICK: f32 = 4.0;
const LABEL_FONT_SIZE: f32 = 8.0;
const COMP_STROKE: f32 = 1.5;

// ── ComponentDef ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ComponentDef {
    Input {
        bits: u32,
        width: u8,
    },
    Output,
    Gate {
        op: GateOp,
        n_inputs: usize,
        width: u8,
    },
    Mux {
        data_width: u8,
        sel_width: u8,
    },
    Demux {
        data_width: u8,
        sel_width: u8,
    },
    Reg {
        data_width: u8,
    },
}

impl ComponentDef {
    fn n_inputs(&self) -> usize {
        match self {
            Self::Input { .. } => 0,
            Self::Output => 1,
            Self::Gate { n_inputs, .. } => *n_inputs,
            Self::Mux { sel_width, .. } => (1usize << sel_width) + 1,
            Self::Demux { .. } => 2,
            Self::Reg { .. } => 2,
        }
    }

    fn n_outputs(&self) -> usize {
        match self {
            Self::Input { .. } => 1,
            Self::Output => 0,
            Self::Gate { .. } => 1,
            Self::Mux { .. } => 1,
            Self::Demux { sel_width, .. } => 1usize << sel_width,
            Self::Reg { .. } => 1,
        }
    }

    // Zero-allocation bounding-box size, matching shape().size but without
    // building the full ComponentShape (outline/anchors/bubbles Vecs) just
    // to read one field - used by component_bounding_rect, which is called
    // every frame for hit-testing/selection.
    fn size(&self) -> Vec2 {
        match self {
            Self::Input { .. } | Self::Output => egui::vec2(COMP_WIDTH, COMP_MIN_HEIGHT),
            Self::Gate { op, n_inputs, .. } => gate_size(*op, *n_inputs),
            Self::Mux { sel_width, .. } => mux_size(*sel_width),
            Self::Demux { sel_width, .. } => demux_size(*sel_width),
            Self::Reg { .. } => reg_size(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Input { .. } => "IN",
            Self::Output => "OUT",
            Self::Gate { op, .. } => match op {
                GateOp::And => "AND",
                GateOp::Or => "OR",
                GateOp::Xor => "XOR",
                GateOp::Nand => "NAND",
                GateOp::Nor => "NOR",
                GateOp::Xnor => "XNOR",
                GateOp::Not => "NOT",
            },
            Self::Mux { .. } => "MUX",
            Self::Demux { .. } => "DEMUX",
            Self::Reg { .. } => "REG",
        }
    }

    fn make_component(&self) -> Component {
        match self {
            Self::Input { bits, width } => Component::input(*bits, *width),
            Self::Output => Component::output(),
            Self::Gate {
                op,
                n_inputs,
                width,
            } => Component::gate(*op, *n_inputs, *width),
            Self::Mux {
                data_width,
                sel_width,
            } => Component::mux(*data_width, *sel_width),
            Self::Demux {
                data_width,
                sel_width,
            } => Component::demux(*data_width, *sel_width),
            Self::Reg { data_width } => Component::reg(*data_width),
        }
    }

    pub fn shape(&self) -> ComponentShape {
        match self {
            Self::Input { .. } => {
                let h = COMP_MIN_HEIGHT;
                ComponentShape {
                    size: egui::vec2(COMP_WIDTH, h),
                    outline: rect_outline(),
                    fill_outline: None,
                    input_anchors: vec![],
                    output_anchors: vec![PinAnchor::right(0.5)],
                    extra_strokes: vec![],
                    output_bubbles: vec![false],
                    labels: vec![],
                    dynamic_label_pos: Vec2::ZERO,
                }
            }
            Self::Output => {
                let h = COMP_MIN_HEIGHT;
                ComponentShape {
                    size: egui::vec2(COMP_WIDTH, h),
                    outline: rect_outline(),
                    fill_outline: None,
                    input_anchors: vec![PinAnchor::left(0.5)],
                    output_anchors: vec![],
                    extra_strokes: vec![],
                    output_bubbles: vec![],
                    labels: vec![],
                    dynamic_label_pos: Vec2::ZERO,
                }
            }
            Self::Gate { op, n_inputs, .. } => gate_shape(*op, *n_inputs),
            Self::Mux { sel_width, .. } => mux_shape(*sel_width),
            Self::Demux { sel_width, .. } => demux_shape(*sel_width),
            Self::Reg { .. } => reg_shape(),
        }
    }
}

// ── PlacedComponent ───────────────────────────────────────────────────────────

pub struct PlacedComponent {
    pub key: CompKey,
    pub def: ComponentDef,
    pub grid_pos: [i32; 2],
}

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
    Component(CompKey),
    Tunnel(TunnelKey),
}

// The origin of an in-progress wire drag: either a component's output pin
// or a Feed tunnel's pin (Feed tunnels behave like Input for dragging
// purposes - valid drag-start only; Pull tunnels behave like Output -
// valid drop-target only, checked separately at drag_stopped()).
#[derive(Debug, Clone, Copy)]
pub enum DragSource {
    Component(CompKey, OutIdx),
    Tunnel(TunnelKey),
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

pub struct OsmilogApp {
    pub circuit: Circuit,
    pub components: Vec<PlacedComponent>,
    pub tunnels: Vec<PlacedTunnel>,
    pub wires: Vec<Wire>,
    pub tunnel_wires: Vec<TunnelWire>,
    pub mode: InteractionMode,
    pub pan: Vec2,
    pub selected: Option<Selected>,
    pub last_settle_error: Option<String>,
}

impl OsmilogApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            circuit: Circuit::new(),
            components: Vec::new(),
            tunnels: Vec::new(),
            wires: Vec::new(),
            tunnel_wires: Vec::new(),
            mode: InteractionMode::Idle,
            pan: Vec2::ZERO,
            selected: None,
            last_settle_error: None,
        }
    }

    fn record_settle_result<T>(&mut self, result: Result<T, crate::circuit::SettleError>) {
        match result {
            Ok(_) => self.last_settle_error = None,
            Err(e) => self.last_settle_error = Some(e.to_string()),
        }
    }

    fn place_component(&mut self, def: ComponentDef, grid_pos: [i32; 2]) {
        let comp = def.make_component();
        let key = self.circuit.add_component(comp);
        self.components.push(PlacedComponent { key, def, grid_pos });
    }

    fn place_tunnel(&mut self, role: TunnelRole, grid_pos: [i32; 2]) {
        let label = format!("Tunnel{}", self.tunnels.len());
        let key = self.circuit.add_tunnel(label.clone(), role);
        self.tunnels.push(PlacedTunnel {
            key,
            label,
            role,
            grid_pos,
        });
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
    }

    fn show_tunnel_properties(&mut self, key: TunnelKey, ui: &mut egui::Ui) {
        let Some(idx) = self.tunnels.iter().position(|pt| pt.key == key) else {
            self.selected = None;
            return;
        };

        ui.heading(match self.tunnels[idx].role {
            TunnelRole::Feed => "TUNNEL (FEED)",
            TunnelRole::Pull => "TUNNEL (PULL)",
        });
        ui.separator();
        ui.label("Label:");
        let mut label = self.tunnels[idx].label.clone();
        let response = ui.text_edit_singleline(&mut label);
        if response.changed() {
            self.tunnels[idx].label = label.clone();
        }

        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            self.circuit.rename_tunnel(key, label.clone());
            self.tunnels[idx].label = label;
            let result = self.circuit.settle();
            self.record_settle_result(result);
        }
    }

    // TODO: can't this be an index for a PlacedComponent instead?
    fn show_component_properties(&mut self, key: CompKey, ui: &mut egui::Ui) {
        let Some(idx) = self.components.iter().position(|pc| pc.key == key) else {
            self.selected = None;
            return;
        };

        ui.heading(self.components[idx].def.label());
        ui.separator();

        let def = self.components[idx].def.clone();
        match def {
            ComponentDef::Input {
                mut bits,
                mut width,
            } => {
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
                            .add(egui::DragValue::new(&mut bits).range(1..=Value::mask(width)))
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
                    self.reconfigure_component(key, ComponentDef::Input { bits, width });
                }
            }
            ComponentDef::Output => {
                let val = self.circuit.read_output(key);
                let val_str = match val {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
            ComponentDef::Gate {
                op,
                mut n_inputs,
                mut width,
            } => {
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
                        ComponentDef::Gate {
                            op,
                            n_inputs,
                            width,
                        },
                    );
                }
            }
            ComponentDef::Mux {
                mut data_width,
                mut sel_width,
            } => {
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
                        ComponentDef::Mux {
                            data_width,
                            sel_width,
                        },
                    );
                }
            }
            ComponentDef::Demux {
                mut data_width,
                mut sel_width,
            } => {
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
                        ComponentDef::Demux {
                            data_width,
                            sel_width,
                        },
                    );
                }
            }
            ComponentDef::Reg { mut data_width } => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Data width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut data_width).range(1..=32))
                        .changed();
                });
                if changed {
                    self.reconfigure_component(key, ComponentDef::Reg { data_width });
                }

                let cur = self.circuit.components[key].pins.out_cache[0];
                let val_str = match cur {
                    Value::Fixed { bits, width } => format!("0x{:X} ({}b)", bits, width),
                    Value::Floating => "Floating".to_string(),
                };
                ui.label(format!("Value: {}", val_str));
            }
        }
    }

    // TODO: write docs, return result
    fn reconfigure_component(&mut self, old_key: CompKey, new_def: ComponentDef) {
        let Some(pc_idx) = self.components.iter().position(|pc| pc.key == old_key) else {
            return;
        };
        let grid_pos = self.components[pc_idx].grid_pos;

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
        self.components[pc_idx] = PlacedComponent {
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
        self.selected = Some(Selected::Component(new_key));
    }
}

impl eframe::App for OsmilogApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            #[cfg(not(target_arch = "wasm32"))]
            std::process::exit(0);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ── Menu bar ──────────────────────────────────────────────────────
        egui::Panel::top("menu_bar").show(ui, |ui| {
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
                                def: ComponentDef::Gate {
                                    op,
                                    n_inputs: n,
                                    width: 1,
                                },
                            };
                            ui.close();
                        }
                    }
                });
                if ui.button("Input").clicked() {
                    self.mode = InteractionMode::Placing {
                        def: ComponentDef::Input { bits: 0, width: 1 },
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
                        def: ComponentDef::Mux {
                            data_width: 1,
                            sel_width: 1,
                        },
                    };
                    ui.close();
                }
                if ui.button("Demux").clicked() {
                    self.mode = InteractionMode::Placing {
                        def: ComponentDef::Demux {
                            data_width: 1,
                            sel_width: 1,
                        },
                    };
                    ui.close();
                }
                ui.menu_button("Memory", |ui| {
                    if ui.button("Register").clicked() {
                        self.mode = InteractionMode::Placing {
                            def: ComponentDef::Reg { data_width: 1 },
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
                ui.colored_label(Color32::RED, err);
            }
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
                        Selected::Component(k) => {
                            if let Some(pc) = self.components.iter_mut().find(|pc| pc.key == k) {
                                pc.grid_pos = original_grid_pos;
                            }
                        }
                        Selected::Tunnel(k) => {
                            if let Some(pt) = self.tunnels.iter_mut().find(|pt| pt.key == k) {
                                pt.grid_pos = original_grid_pos;
                            }
                        }
                    }
                }
                self.mode = InteractionMode::Idle;
            }

            draw_grid(&painter, clip_rect, pan);

            // Draw wires. Resolves the net live via Component::net_of rather
            // than caching a NetKey on Wire: a cached key can go stale if a
            // later link() merges its net away (merge() always removes one
            // of the two merged NetKeys), which would otherwise panic here
            // the very next frame.
            for wire in &self.wires {
                let (p0, p1) = {
                    let src = self
                        .components
                        .iter()
                        .find(|pc| pc.key == wire.src_comp)
                        .unwrap();
                    let dst = self
                        .components
                        .iter()
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
                    .map(|nk| value_stroke(self.circuit.nets[nk].value))
                    .unwrap_or(value_stroke(Value::Floating));
                draw_wire(&painter, p0, p1, color);
            }

            // Draw tunnel wires (component pin <-> tunnel), same live-lookup
            // approach as above - no cached NetKey.
            for tw in &self.tunnel_wires {
                let (Some(pt), Some(pc)) = (
                    self.tunnels.iter().find(|pt| pt.key == tw.tunnel),
                    self.components.iter().find(|pc| pc.key == tw.comp),
                ) else {
                    continue;
                };
                let p0 = tunnel_pin_pos(pt, pan);
                let p1 = comp_pin_pos(&pc.def.shape(), pc.grid_pos, pan, tw.pin);
                let net = self.circuit.components[tw.comp].net_of(tw.pin);
                let color = net
                    .map(|nk| value_stroke(self.circuit.nets[nk].value))
                    .unwrap_or(value_stroke(Value::Floating));
                draw_wire(&painter, p0, p1, color);
            }

            // Draw components
            for pc in &self.components {
                let is_selected = self.selected == Some(Selected::Component(pc.key));
                draw_component(&painter, pc, pan, &self.circuit, is_selected);
            }

            // Draw tunnels
            for pt in &self.tunnels {
                let is_selected = self.selected == Some(Selected::Tunnel(pt.key));
                draw_tunnel(&painter, pt, pan, &self.circuit, is_selected);
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
                                pin_at_pos(&self.components, pan, pos, PinKind::Output)
                            {
                                // Output-pin drag → start wire (highest priority)
                                self.mode = InteractionMode::WireDrag {
                                    src: DragSource::Component(comp_key, out_idx),
                                    current_end: pos,
                                };
                            } else if let Some(tunnel_key) =
                                tunnel_pin_at_pos(&self.tunnels, pan, pos)
                            {
                                // Feed-tunnel pin drag → start wire
                                self.mode = InteractionMode::WireDrag {
                                    src: DragSource::Tunnel(tunnel_key),
                                    current_end: pos,
                                };
                            } else if let Some(sel) = self.selected {
                                // Selected component/tunnel body drag → move it
                                let hit_pos = match sel {
                                    Selected::Component(key) => {
                                        self.components.iter().find(|pc| pc.key == key).map(|pc| {
                                            (component_bounding_rect(pc, pan), pc.grid_pos)
                                        })
                                    }
                                    Selected::Tunnel(key) => self
                                        .tunnels
                                        .iter()
                                        .find(|pt| pt.key == key)
                                        .map(|pt| (tunnel_bounding_rect(pt, pan), pt.grid_pos)),
                                };
                                if let Some((rect, grid_pos)) = hit_pos {
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
                    }

                    // Click any component/tunnel body to select it (components
                    // take priority over tunnels on overlap); click empty
                    // canvas to deselect.
                    if response.clicked() {
                        if let Some(pos) = pointer {
                            self.selected = self
                                .components
                                .iter()
                                .find(|pc| component_bounding_rect(pc, pan).contains(pos))
                                .map(|pc| Selected::Component(pc.key))
                                .or_else(|| {
                                    self.tunnels
                                        .iter()
                                        .find(|pt| tunnel_bounding_rect(pt, pan).contains(pos))
                                        .map(|pt| Selected::Tunnel(pt.key))
                                });
                        }
                    }
                }

                InteractionMode::Placing { def } => {
                    if let Some(pos) = pointer {
                        let gp = snap_to_grid(pos, pan);
                        draw_ghost(&painter, &def, gp, pan);
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
                        draw_tunnel_ghost(&painter, role, gp, pan);
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
                        DragSource::Component(comp_key, out_idx) => {
                            let src_pc = self
                                .components
                                .iter()
                                .find(|pc| pc.key == comp_key)
                                .unwrap();
                            comp_pin_pos(
                                &src_pc.def.shape(),
                                src_pc.grid_pos,
                                pan,
                                PinId::output(out_idx.0),
                            )
                        }
                        DragSource::Tunnel(tunnel_key) => {
                            let src_pt =
                                self.tunnels.iter().find(|pt| pt.key == tunnel_key).unwrap();
                            tunnel_pin_pos(src_pt, pan)
                        }
                    };

                    let end = pointer.unwrap_or(current_end);
                    let stroke = Stroke::new(WIRE_THICKNESS_THIN, Color32::from_gray(150));
                    draw_wire(&painter, p0, end, stroke);

                    self.mode = InteractionMode::WireDrag {
                        src,
                        current_end: end,
                    };

                    if response.drag_stopped() {
                        let target = pin_at_pos(&self.components, pan, end, PinKind::Input);
                        match (src, target) {
                            (
                                DragSource::Component(src_comp, src_pin),
                                Some((dst_comp, PinId::In(in_idx))),
                            ) => {
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
                            (DragSource::Component(src_comp, src_pin), None) => {
                                // Didn't land on a component input pin; check
                                // whether it landed on a Pull tunnel instead.
                                let tunnel_key = self
                                    .tunnels
                                    .iter()
                                    .find(|pt| {
                                        pt.role == TunnelRole::Pull
                                            && tunnel_bounding_rect(pt, pan).contains(end)
                                    })
                                    .map(|pt| pt.key);
                                if let Some(tunnel_key) = tunnel_key {
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
                            (
                                DragSource::Tunnel(tunnel_key),
                                Some((dst_comp, PinId::In(in_idx))),
                            ) => {
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
                            Selected::Component(k) => {
                                if let Some(pc) = self.components.iter_mut().find(|pc| pc.key == k)
                                {
                                    pc.grid_pos = new_grid_pos;
                                }
                            }
                            Selected::Tunnel(k) => {
                                if let Some(pt) = self.tunnels.iter_mut().find(|pt| pt.key == k) {
                                    pt.grid_pos = new_grid_pos;
                                }
                            }
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

fn pin_at_pos(
    components: &[PlacedComponent],
    pan: Vec2,
    pos: Pos2,
    kind: PinKind,
) -> Option<(CompKey, PinId)> {
    let hit_r = PIN_RADIUS * 2.0;
    for pc in components {
        let shape = pc.def.shape();
        match kind {
            PinKind::Output => {
                for i in 0..pc.def.n_outputs() {
                    let pp = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::output(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((pc.key, PinId::output(i as u8)));
                    }
                }
            }
            PinKind::Input => {
                for i in 0..pc.def.n_inputs() {
                    let pp = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::input(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((pc.key, PinId::input(i as u8)));
                    }
                }
            }
        }
    }
    None
}
fn tunnel_pin_at_pos(tunnels: &[PlacedTunnel], pan: Vec2, pos: Pos2) -> Option<TunnelKey> {
    let hit_r = PIN_RADIUS * 2.0;
    for tunnel in tunnels {
        if tunnel.role == TunnelRole::Feed && tunnel_pin_pos(tunnel, pan).distance(pos) <= hit_r {
            return Some(tunnel.key);
        }
    }
    None
}

// ── Color ─────────────────────────────────────────────────────────────────────

fn value_stroke(val: Value) -> Stroke {
    let (color, weight) = match val {
        Value::Floating => (Color32::GRAY, WIRE_THICKNESS_THIN),
        Value::Fixed { bits, width } => (
            if bits == 0 {
                Color32::from_rgb(40, 40, 80)
            } else {
                Color32::from_rgb(50, 200, 80)
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

fn draw_grid(painter: &Painter, clip_rect: Rect, pan: Vec2) {
    let x0 = ((clip_rect.left() - pan.x) / GRID_SIZE).floor() as i32;
    let x1 = ((clip_rect.right() - pan.x) / GRID_SIZE).ceil() as i32;
    let y0 = ((clip_rect.top() - pan.y) / GRID_SIZE).floor() as i32;
    let y1 = ((clip_rect.bottom() - pan.y) / GRID_SIZE).ceil() as i32;
    for gx in x0..=x1 {
        for gy in y0..=y1 {
            painter.circle_filled(
                egui::pos2(gx as f32 * GRID_SIZE + pan.x, gy as f32 * GRID_SIZE + pan.y),
                1.0,
                Color32::from_gray(60),
            );
        }
    }
}

fn draw_wire(painter: &Painter, p0: Pos2, p1: Pos2, stroke: Stroke) {
    let mid_x = (p0.x + p1.x) / 2.0;
    let elbow1 = egui::pos2(mid_x, p0.y);
    let elbow2 = egui::pos2(mid_x, p1.y);
    painter.line_segment([p0, elbow1], stroke);
    painter.line_segment([elbow1, elbow2], stroke);
    painter.line_segment([elbow2, p1], stroke);
}

fn draw_component(
    painter: &Painter,
    pc: &PlacedComponent,
    pan: Vec2,
    circuit: &Circuit,
    is_selected: bool,
) {
    let shape = pc.def.shape();
    let rect = component_bounding_rect(pc, pan);
    let fill = Color32::from_rgb(45, 45, 65);
    let (stroke_w, stroke_col) = if is_selected {
        (COMP_STROKE + 1.0, Color32::from_rgb(100, 160, 255))
    } else {
        (COMP_STROKE, Color32::from_gray(160))
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
            Color32::WHITE,
        );
    }

    for i in 0..pc.def.n_inputs() {
        let pos = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::input(i as u8));
        let val = circuit.components[pc.key].pins.inputs[i]
            .map(|nk| circuit.nets[nk].value)
            .unwrap_or(Value::Floating);
        painter.circle_filled(pos, PIN_RADIUS, value_stroke(val).color);
    }
    for i in 0..pc.def.n_outputs() {
        let pos = comp_pin_pos(&shape, pc.grid_pos, pan, PinId::output(i as u8));
        let val = circuit.components[pc.key].pins.out_cache[i];
        painter.circle_filled(pos, PIN_RADIUS, value_stroke(val).color);
    }
}

fn draw_tunnel(
    painter: &Painter,
    pt: &PlacedTunnel,
    pan: Vec2,
    circuit: &Circuit,
    is_selected: bool,
) {
    let shape = tunnel_shape(pt.role);
    let rect = tunnel_bounding_rect(pt, pan);
    // Slightly different tint from components, to visually distinguish tunnels.
    let fill = Color32::from_rgb(65, 45, 65);
    let (stroke_w, stroke_col) = if is_selected {
        (COMP_STROKE + 1.0, Color32::from_rgb(100, 160, 255))
    } else {
        (COMP_STROKE, Color32::from_gray(160))
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
        Color32::WHITE,
    );

    let val = circuit
        .tunnels
        .get(pt.key)
        .and_then(|t| t.net)
        .map(|nk| circuit.nets[nk].value)
        .unwrap_or(Value::Floating);
    painter.circle_filled(tunnel_pin_pos(pt, pan), PIN_RADIUS, value_stroke(val).color);
}

fn draw_ghost(painter: &Painter, def: &ComponentDef, grid_pos: [i32; 2], pan: Vec2) {
    let shape = def.shape();
    let tl = egui::pos2(
        grid_pos[0] as f32 * GRID_SIZE + pan.x,
        grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, shape.size);
    let ghost_col = Color32::from_gray(120);

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

fn draw_tunnel_ghost(painter: &Painter, role: TunnelRole, grid_pos: [i32; 2], pan: Vec2) {
    let shape = tunnel_shape(role);
    let tl = egui::pos2(
        grid_pos[0] as f32 * GRID_SIZE + pan.x,
        grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, shape.size);
    let ghost_col = Color32::from_gray(120);

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

use eframe;
use egui::epaint::{PathShape, PathStroke};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, Vec2};

use crate::{
    circuit::Circuit,
    component::{CompKey, Component, GateOp, InIdx, OutIdx, PinId},
    geometry::{
        demux_shape, gate_shape, mux_shape, rect_outline, reg_shape, snap_to_grid, COMP_MIN_HEIGHT,
        COMP_WIDTH, GRID_SIZE,
    },
    net::NetKey,
    shape::{tessellate_path, ComponentShape, PinAnchor, BUBBLE_R},
    value::Value,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const PIN_RADIUS: f32 = 3.0;
const WIRE_THICKNESS: f32 = 2.0;

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
                    label_norm: egui::vec2(0.5, 0.5),
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
                    label_norm: egui::vec2(0.5, 0.5),
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
    pub label: String,
}

// ── Wire ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct Wire {
    pub net_key: NetKey,
    pub src_comp: CompKey,
    pub src_pin: OutIdx,
    pub dst_comp: CompKey,
    pub dst_pin: InIdx,
}

// ── InteractionMode ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum InteractionMode {
    Idle,
    Placing {
        def: ComponentDef,
    },
    WireDrag {
        src_comp: CompKey,
        src_pin: OutIdx,
        current_end: Pos2,
    },
    ComponentDrag {
        key: CompKey,
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
    pub wires: Vec<Wire>,
    pub mode: InteractionMode,
    pub pan: Vec2,
    pub selected: Option<CompKey>,
}

impl OsmilogApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            circuit: Circuit::new(),
            components: Vec::new(),
            wires: Vec::new(),
            mode: InteractionMode::Idle,
            pan: Vec2::ZERO,
            selected: None,
        }
    }

    fn place_component(&mut self, def: ComponentDef, grid_pos: [i32; 2]) {
        let comp = def.make_component();
        let key = self.circuit.add_component(comp);
        let label = format!("{}{}", def.label(), self.components.len());
        self.components.push(PlacedComponent {
            key,
            def,
            grid_pos,
            label,
        });
    }

    /// Shows property menu for the currently selected component. ComponentDef for the UI element is
    /// cloned. If the user edits a property, call `self.reconfigure_component()` with an updated ComponentDef
    fn show_properties(&mut self, ui: &mut egui::Ui) {
        // TODO: can't this be an index for a PlacedComponent instead?
        let Some(key) = self.selected else {
            ui.label("Click a component to select it.");
            return;
        };
        let Some(idx) = self.components.iter().position(|pc| pc.key == key) else {
            self.selected = None;
            return;
        };

        ui.heading(self.components[idx].def.label());
        ui.separator();
        ui.label("Label:");
        ui.text_edit_singleline(&mut self.components[idx].label);
        ui.separator();

        // TODO: what's up with this clone
        let def = self.components[idx].def.clone();
        match def {
            ComponentDef::Input {
                mut bits,
                mut width,
            } => {
                // FIXME: This needs to update ComponentDef
                let mut changed = false;
                let val_str = format!("0x{:X} ({}b)", bits, width);
                ui.label(format!("Value: {}", val_str));

                // TODO: Checkbox if width == 1 else input field
                if ui.button("Toggle").clicked() {
                    bits = (bits + 1) & Value::mask(width);
                    changed = true;
                    // self.circuit.set_input(key, bits, width);
                    // self.circuit.settle();
                }
                ui.horizontal(|ui| {
                    ui.label("Width:");
                    changed |= ui
                        .add(egui::DragValue::new(&mut width).range(1..=32))
                        .changed();
                });
                if changed {
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
        let label = self.components[pc_idx].label.clone();

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
            .copied()
            .collect();

        self.circuit.remove_component(old_key);
        self.wires
            .retain(|w| w.src_comp != old_key && w.dst_comp != old_key);

        let new_key = self.circuit.add_component(new_comp);
        self.components[pc_idx] = PlacedComponent {
            key: new_key,
            def: new_def,
            grid_pos,
            label,
        };

        for w in surviving {
            let (src, src_pin, dst, dst_pin) = if w.src_comp == old_key {
                (new_key, w.src_pin, w.dst_comp, w.dst_pin)
            } else {
                (w.src_comp, w.src_pin, new_key, w.dst_pin)
            };
            let net_key =
                self.circuit
                    .link(src, PinId::output(src_pin.0), dst, PinId::input(dst_pin.0));
            self.wires.push(Wire {
                net_key,
                src_comp: src,
                src_pin,
                dst_comp: dst,
                dst_pin,
            });
        }

        self.circuit.settle();
        self.selected = Some(new_key);
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
            });
            if ui.button("Tick Clock").clicked() {
                self.circuit.tick_clock();
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
                    if let Some(pc) = self.components.iter_mut().find(|pc| pc.key == key) {
                        pc.grid_pos = original_grid_pos;
                    }
                }
                self.mode = InteractionMode::Idle;
            }

            draw_grid(&painter, clip_rect, pan);

            // Draw wires
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
                        pin_pos(src, pan, PinId::output(wire.src_pin.0)),
                        pin_pos(dst, pan, PinId::input(wire.dst_pin.0)),
                    )
                };
                let color = value_color(self.circuit.nets[wire.net_key].value);
                draw_wire(&painter, p0, p1, color);
            }

            // Draw components
            for pc in &self.components {
                let is_selected = self.selected == Some(pc.key);
                draw_component(&painter, pc, pan, &self.circuit, is_selected);
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
                                    src_comp: comp_key,
                                    src_pin: out_idx,
                                    current_end: pos,
                                };
                            } else if let Some(sel_key) = self.selected {
                                // Selected component body drag → move component
                                if let Some(pc) =
                                    self.components.iter().find(|pc| pc.key == sel_key)
                                {
                                    if component_bounding_rect(pc, pan).contains(pos) {
                                        self.mode = InteractionMode::ComponentDrag {
                                            key: sel_key,
                                            drag_origin: pos,
                                            original_grid_pos: pc.grid_pos,
                                        };
                                    }
                                }
                            }
                        }
                    }

                    // Click any component body to select it; click empty canvas to deselect
                    if response.clicked() {
                        if let Some(pos) = pointer {
                            self.selected = self
                                .components
                                .iter()
                                .find(|pc| component_bounding_rect(pc, pan).contains(pos))
                                .map(|pc| pc.key);
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

                InteractionMode::WireDrag {
                    src_comp,
                    src_pin,
                    current_end,
                } => {
                    let p0 = {
                        let src_pc = self
                            .components
                            .iter()
                            .find(|pc| pc.key == src_comp)
                            .unwrap();
                        pin_pos(src_pc, pan, PinId::output(src_pin.0))
                    };

                    let end = pointer.unwrap_or(current_end);
                    draw_wire(&painter, p0, end, Color32::from_gray(150));

                    self.mode = InteractionMode::WireDrag {
                        src_comp,
                        src_pin,
                        current_end: end,
                    };

                    if response.drag_stopped() {
                        let target = pin_at_pos(&self.components, pan, end, PinKind::Input);
                        if let Some((dst_comp, PinId::In(in_idx))) = target {
                            if dst_comp != src_comp {
                                let net = self.circuit.link(
                                    src_comp,
                                    PinId::output(src_pin.0),
                                    dst_comp,
                                    PinId::input(in_idx.0),
                                );
                                self.circuit.settle();
                                self.wires.push(Wire {
                                    net_key: net,
                                    src_comp,
                                    src_pin,
                                    dst_comp,
                                    dst_pin: in_idx,
                                });
                            }
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
                        if let Some(pc) = self.components.iter_mut().find(|pc| pc.key == key) {
                            pc.grid_pos = new_grid_pos;
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
    let size = pc.def.shape().size;
    let tl = egui::pos2(
        pc.grid_pos[0] as f32 * GRID_SIZE + pan.x,
        pc.grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    Rect::from_min_size(tl, size)
}

fn pin_pos(pc: &PlacedComponent, pan: Vec2, pin: PinId) -> Pos2 {
    let shape = pc.def.shape();
    let tl = egui::pos2(
        pc.grid_pos[0] as f32 * GRID_SIZE + pan.x,
        pc.grid_pos[1] as f32 * GRID_SIZE + pan.y,
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

fn pin_at_pos(
    components: &[PlacedComponent],
    pan: Vec2,
    pos: Pos2,
    kind: PinKind,
) -> Option<(CompKey, PinId)> {
    let hit_r = PIN_RADIUS * 2.0;
    for pc in components {
        match kind {
            PinKind::Output => {
                for i in 0..pc.def.n_outputs() {
                    let pp = pin_pos(pc, pan, PinId::output(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((pc.key, PinId::output(i as u8)));
                    }
                }
            }
            PinKind::Input => {
                for i in 0..pc.def.n_inputs() {
                    let pp = pin_pos(pc, pan, PinId::input(i as u8));
                    if pos.distance(pp) <= hit_r {
                        return Some((pc.key, PinId::input(i as u8)));
                    }
                }
            }
        }
    }
    None
}

// ── Color ─────────────────────────────────────────────────────────────────────

fn value_color(val: Value) -> Color32 {
    match val {
        Value::Floating => Color32::GRAY,
        Value::Fixed { bits: 0, .. } => Color32::from_rgb(40, 40, 80),
        Value::Fixed { .. } => Color32::from_rgb(50, 200, 80),
    }
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

fn draw_wire(painter: &Painter, p0: Pos2, p1: Pos2, color: Color32) {
    let stroke = Stroke::new(WIRE_THICKNESS, color);
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
        (2.5_f32, Color32::from_rgb(100, 160, 255))
    } else {
        (1.5_f32, Color32::from_gray(160))
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

    let lp = egui::pos2(
        rect.left() + shape.label_norm.x * rect.width(),
        rect.top() + shape.label_norm.y * rect.height(),
    );
    painter.text(
        lp,
        Align2::CENTER_CENTER,
        &pc.label,
        FontId::monospace(11.0),
        Color32::WHITE,
    );

    for i in 0..pc.def.n_inputs() {
        let pos = pin_pos(pc, pan, PinId::input(i as u8));
        let val = circuit.components[pc.key].pins.inputs[i]
            .map(|nk| circuit.nets[nk].value)
            .unwrap_or(Value::Floating);
        painter.circle_filled(pos, PIN_RADIUS, value_color(val));
    }
    for i in 0..pc.def.n_outputs() {
        let pos = pin_pos(pc, pan, PinId::output(i as u8));
        let val = circuit.components[pc.key].pins.out_cache[i];
        painter.circle_filled(pos, PIN_RADIUS, value_color(val));
    }
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
        stroke: PathStroke::new(1.5, ghost_col),
    }));

    for stroke_cmds in &shape.extra_strokes {
        let stroke_pts = tessellate_path(stroke_cmds, rect);
        painter.add(egui::Shape::line(stroke_pts, Stroke::new(1.5, ghost_col)));
    }

    let lp = egui::pos2(
        rect.left() + shape.label_norm.x * rect.width(),
        rect.top() + shape.label_norm.y * rect.height(),
    );
    painter.text(
        lp,
        Align2::CENTER_CENTER,
        def.label(),
        FontId::monospace(11.0),
        ghost_col,
    );
}

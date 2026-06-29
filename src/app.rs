use eframe;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, Vec2};

use crate::{
    circuit::Circuit,
    component::{CompKey, Component, GateOp, InIdx, OutIdx, PinId},
    net::NetKey,
    value::Value,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const GRID_SIZE: f32 = 20.0;
const COMP_WIDTH: f32 = 80.0;
const COMP_HEIGHT_PER_PIN: f32 = 20.0;
const COMP_MIN_HEIGHT: f32 = 40.0;
const PIN_RADIUS: f32 = 5.0;
const WIRE_THICKNESS: f32 = 2.0;

// ── ComponentDef ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ComponentDef {
    Input { value: Value },
    Output,
    Gate { op: GateOp, n_inputs: usize, width: u8 },
    Mux { data_width: u8, sel_width: u8 },
    Demux { data_width: u8, sel_width: u8 },
}

impl ComponentDef {
    fn n_inputs(&self) -> usize {
        match self {
            Self::Input { .. } => 0,
            Self::Output => 1,
            Self::Gate { n_inputs, .. } => *n_inputs,
            Self::Mux { sel_width, .. } => (1usize << sel_width) + 1,
            Self::Demux { .. } => 2,
        }
    }

    fn n_outputs(&self) -> usize {
        match self {
            Self::Input { .. } => 1,
            Self::Output => 0,
            Self::Gate { .. } => 1,
            Self::Mux { .. } => 1,
            Self::Demux { sel_width, .. } => 1usize << sel_width,
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
        }
    }

    fn make_component(&self) -> Component {
        match self {
            Self::Input { value } => Component::input(*value),
            Self::Output => Component::output(),
            Self::Gate { op, n_inputs, width } => Component::gate(*op, *n_inputs, *width),
            Self::Mux { data_width, sel_width } => Component::mux(*data_width, *sel_width),
            Self::Demux { data_width, sel_width } => Component::demux(*data_width, *sel_width),
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
    Placing { def: ComponentDef },
    WireDrag { src_comp: CompKey, src_pin: OutIdx, current_end: Pos2 },
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
}

impl OsmilogApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            circuit: Circuit::new(),
            components: Vec::new(),
            wires: Vec::new(),
            mode: InteractionMode::Idle,
            pan: Vec2::ZERO,
        }
    }

    fn place_component(&mut self, def: ComponentDef, grid_pos: [i32; 2]) {
        let comp = def.make_component();
        let key = self.circuit.add_component(comp);
        let label = format!("{}{}", def.label(), self.components.len());
        self.components.push(PlacedComponent { key, def, grid_pos, label });
    }
}

impl eframe::App for OsmilogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ── Menu bar ──────────────────────────────────────────────────────
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.menu_button("Add", |ui: &mut egui::Ui| {
                ui.menu_button("Gates", |ui: &mut egui::Ui| {
                    let gates: [(&str, GateOp, usize); 6] = [
                        ("AND",  GateOp::And,  2),
                        ("OR",   GateOp::Or,   2),
                        ("XOR",  GateOp::Xor,  2),
                        ("NAND", GateOp::Nand, 2),
                        ("NOR",  GateOp::Nor,  2),
                        ("NOT",  GateOp::Not,  1),
                    ];
                    for (name, op, n) in gates {
                        if ui.button(name).clicked() {
                            self.mode = InteractionMode::Placing {
                                def: ComponentDef::Gate { op, n_inputs: n, width: 1 },
                            };
                            ui.close();
                        }
                    }
                });
                if ui.button("Input").clicked() {
                    self.mode = InteractionMode::Placing {
                        def: ComponentDef::Input { value: Value::new(0, 1) },
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
                        def: ComponentDef::Mux { data_width: 1, sel_width: 1 },
                    };
                    ui.close();
                }
                if ui.button("Demux").clicked() {
                    self.mode = InteractionMode::Placing {
                        def: ComponentDef::Demux { data_width: 1, sel_width: 1 },
                    };
                    ui.close();
                }
            });
        });

        ui.separator();

        // ── Canvas ────────────────────────────────────────────────────────
        {
            let (response, painter) =
                ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
            let clip_rect = painter.clip_rect();
            let pan = self.pan;

            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.mode = InteractionMode::Idle;
            }

            draw_grid(&painter, clip_rect, pan);

            // Draw wires
            for wire in &self.wires {
                let (p0, p1) = {
                    let src = self.components.iter().find(|pc| pc.key == wire.src_comp).unwrap();
                    let dst = self.components.iter().find(|pc| pc.key == wire.dst_comp).unwrap();
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
                draw_component(&painter, pc, pan, &self.circuit);
            }

            let pointer = response.interact_pointer_pos()
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
                                self.mode = InteractionMode::WireDrag {
                                    src_comp: comp_key,
                                    src_pin: out_idx,
                                    current_end: pos,
                                };
                            }
                        }
                    }

                    // Click on Input component body to toggle value
                    if response.clicked() {
                        if let Some(pos) = pointer {
                            let clicked_key = self.components.iter()
                                .filter(|pc| matches!(pc.def, ComponentDef::Input { .. }))
                                .find(|pc| component_rect(pc, pan).contains(pos))
                                .map(|pc| pc.key);
                            if let Some(key) = clicked_key {
                                let cur = self.circuit.components[key].pins.out_cache[0];
                                let next = match cur {
                                    Value::Fixed { bits, width } => Value::new(bits ^ 1, width),
                                    Value::Floating => Value::new(1, 1),
                                };
                                self.circuit.set_input(key, next);
                                self.circuit.settle();
                            }
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

                InteractionMode::WireDrag { src_comp, src_pin, current_end } => {
                    let p0 = {
                        let src_pc = self.components.iter()
                            .find(|pc| pc.key == src_comp).unwrap();
                        pin_pos(src_pc, pan, PinId::output(src_pin.0))
                    };

                    let end = pointer.unwrap_or(current_end);
                    draw_wire(&painter, p0, end, Color32::from_gray(150));

                    // Keep current_end updated while dragging
                    self.mode = InteractionMode::WireDrag { src_comp, src_pin, current_end: end };

                    if response.drag_stopped() {
                        let target = pin_at_pos(&self.components, pan, end, PinKind::Input);
                        if let Some((dst_comp, PinId::In(in_idx))) = target {
                            if dst_comp != src_comp {
                                let net = self.circuit.link(
                                    src_comp, PinId::output(src_pin.0),
                                    dst_comp,  PinId::input(in_idx.0),
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
            }
        }
    }
}

// ── Geometry ─────────────────────────────────────────────────────────────────

fn component_rect(pc: &PlacedComponent, pan: Vec2) -> Rect {
    let n_slots = pc.def.n_inputs().max(pc.def.n_outputs()).max(1);
    let h = (n_slots as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT);
    let tl = egui::pos2(
        pc.grid_pos[0] as f32 * GRID_SIZE + pan.x,
        pc.grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    Rect::from_min_size(tl, egui::vec2(COMP_WIDTH, h))
}

fn pin_pos(pc: &PlacedComponent, pan: Vec2, pin: PinId) -> Pos2 {
    let rect = component_rect(pc, pan);
    match pin {
        PinId::In(InIdx(i)) => {
            let n = pc.def.n_inputs();
            let frac = (i as f32 + 1.0) / (n as f32 + 1.0);
            egui::pos2(rect.left(), rect.top() + frac * rect.height())
        }
        PinId::Out(OutIdx(i)) => {
            let n = pc.def.n_outputs();
            let frac = (i as f32 + 1.0) / (n as f32 + 1.0);
            egui::pos2(rect.right(), rect.top() + frac * rect.height())
        }
    }
}

fn snap_to_grid(pos: Pos2, pan: Vec2) -> [i32; 2] {
    [
        ((pos.x - pan.x) / GRID_SIZE).round() as i32,
        ((pos.y - pan.y) / GRID_SIZE).round() as i32,
    ]
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

fn draw_component(painter: &Painter, pc: &PlacedComponent, pan: Vec2, circuit: &Circuit) {
    let rect = component_rect(pc, pan);

    painter.rect_filled(rect, 4.0, Color32::from_rgb(45, 45, 65));
    painter.rect_stroke(rect, 4.0, Stroke::new(1.5, Color32::from_gray(160)), egui::StrokeKind::Outside);
    painter.text(
        rect.center(),
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
    let n_slots = def.n_inputs().max(def.n_outputs()).max(1);
    let h = (n_slots as f32 * COMP_HEIGHT_PER_PIN).max(COMP_MIN_HEIGHT);
    let tl = egui::pos2(
        grid_pos[0] as f32 * GRID_SIZE + pan.x,
        grid_pos[1] as f32 * GRID_SIZE + pan.y,
    );
    let rect = Rect::from_min_size(tl, egui::vec2(COMP_WIDTH, h));
    painter.rect_stroke(rect, 4.0, Stroke::new(1.5, Color32::from_gray(120)), egui::StrokeKind::Outside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        def.label(),
        FontId::monospace(11.0),
        Color32::from_gray(120),
    );
}

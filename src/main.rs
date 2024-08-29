use std::collections::HashMap;

use macroquad::ui::widgets::{Button, Group, TreeNode};
use macroquad::ui::{hash, root_ui, Skin, Ui};
use macroquad::{prelude::*, ui::widgets::Window};

use petgraph::algo::toposort;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::Direction::Incoming;
use std::fmt::Debug;

use bitvec::prelude::*;

type Signal = BitVec<u32, Lsb0>;

fn signal_zeros(n: u8) -> Signal {
    bitvec![u32, Lsb0; 0; n as usize]
}

trait Logic {
    fn name(&self) -> &'static str;
    fn n_in_pins(&self) -> usize;
    fn n_out_pins(&self) -> usize;
    fn get_pin_value(&self, px: PinIndex) -> &Signal;
    fn set_pin_value(&mut self, px: PinIndex, value: &Signal);
    fn do_logic(&mut self);
    fn is_clocked(&self) -> bool {
        false
    }
    fn tick_clock(&mut self) {}
    // Any additional changes that happen when you click on the component
    fn interact(&mut self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
enum GateKind {
    Not,
    Or,
    And,
}

#[derive(Debug, Clone)]
struct Gate {
    kind: GateKind,
    data_bits: u8,
    n_inputs: usize,
    inputs: Vec<Signal>,
    output: Signal,
}

impl Logic for Gate {
    fn name(&self) -> &'static str {
        match self.kind {
            GateKind::Not => "Gate: NOT",
            GateKind::Or => "Gate: OR",
            GateKind::And => "Gate: AND",
        }
    }
    fn n_in_pins(&self) -> usize {
        self.n_inputs
    }
    fn n_out_pins(&self) -> usize {
        1
    }

    fn get_pin_value(&self, px: PinIndex) -> &Signal {
        match px {
            PinIndex::Input(i) => &self.inputs[i],
            PinIndex::Output(i) => {
                if i == 0 {
                    &self.output
                } else {
                    panic!()
                }
            }
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: &Signal) {
        match px {
            PinIndex::Input(i) => {
                if i < self.n_inputs {
                    self.inputs[i].copy_from_bitslice(value);
                } else {
                    panic!()
                }
            }
            PinIndex::Output(i) => {
                if i == 0 {
                    self.output.copy_from_bitslice(value);
                } else {
                    panic!()
                }
            }
        }
    }

    fn do_logic(&mut self) {
        self.output = match self.kind {
            GateKind::Not => !self.inputs[0].clone(),
            GateKind::Or => self.inputs.iter().fold(signal_zeros(1), |x, y| x | y),
            GateKind::And => self
                .inputs
                .iter()
                .fold(self.inputs[0].clone(), |x, y| x & y),
        };
    }
}

impl Draw for Gate {
    fn size(&self) -> Vec2 {
        let tex_info = self.tex_info();
        tex_info.size / tex_info.scale
    }

    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>) {
        self.draw_from_texture_slice(pos, textures.get("gates").unwrap(), self.tex_info());
    }
    fn input_positions(&self) -> Vec<Vec2> {
        match self.kind {
            GateKind::Not => vec![vec2(0., 20.)],
            GateKind::Or => vec![vec2(0., 10.), vec2(0., 28.)],
            GateKind::And => vec![vec2(0., 8.), vec2(0., 25.)],
        }
    }

    fn output_positions(&self) -> Vec<Vec2> {
        let tex_info = self.tex_info();
        vec![vec2(tex_info.size.x, tex_info.size.y / 2.) / tex_info.scale]
    }
}

impl Gate {
    fn new(kind: GateKind, data_bits: u8, n_inputs: usize) -> Self {
        Self {
            kind,
            data_bits,
            n_inputs,
            inputs: vec![signal_zeros(data_bits); n_inputs as usize],
            output: signal_zeros(data_bits),
        }
    }
    fn default_of_kind(kind: GateKind) -> Self {
        match kind {
            GateKind::Not => Self::new(kind, 1, 1),
            GateKind::Or => Self::new(kind, 1, 2),
            GateKind::And => Self::new(kind, 1, 2),
        }
    }
    fn tex_info(&self) -> TexInfo {
        match self.kind {
            GateKind::Not => TexInfo::new(vec2(448., 111.), vec2(80., 80.), 2.),
            GateKind::And => TexInfo::new(vec2(72., 0.), vec2(90., 69.), 2.),
            GateKind::Or => TexInfo::new(vec2(72., 233.), vec2(90., 78.), 2.),
        }
    }

    fn create_inputs(&self) -> Vec<Signal> {
        vec![signal_zeros(self.data_bits); self.n_inputs as usize]
    }
    fn create_outputs(&self) -> Vec<Signal> {
        vec![signal_zeros(self.data_bits)]
    }
}

#[derive(Debug, Clone)]
struct Mux {
    sel_bits: u8,
    data_bits: u8,
    inputs: Vec<Signal>,
    output: Signal,
    selector: Signal,
}

impl Logic for Mux {
    fn name(&self) -> &'static str {
        "Multiplexer"
    }
    fn do_logic(&mut self) {
        let sel = self.selector.load::<usize>();
        self.output.copy_from_bitslice(&self.inputs[sel]);
    }

    fn n_in_pins(&self) -> usize {
        // Count the inputs and the selector pin
        self.inputs.len() + 1
    }

    fn n_out_pins(&self) -> usize {
        1
    }

    fn get_pin_value(&self, px: PinIndex) -> &Signal {
        match px {
            PinIndex::Input(i) => {
                // 0 -> selector, then inputs
                if i == 0 {
                    &self.selector
                } else {
                    &self.inputs[i - 1]
                }
            }
            PinIndex::Output(i) => {
                if i == 0 {
                    &self.output
                } else {
                    panic!()
                }
            }
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: &Signal) {
        match px {
            PinIndex::Input(i) => {
                if i == 0 {
                    self.selector.copy_from_bitslice(value);
                } else {
                    self.inputs[i - 1].copy_from_bitslice(value)
                }
            }
            PinIndex::Output(i) => {
                if i == 0 {
                    self.output.copy_from_bitslice(value)
                } else {
                    panic!()
                }
            }
        }
    }
}

impl Draw for Mux {
    fn size(&self) -> Vec2 {
        vec2(30., (self.inputs.len() * 20) as f32)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();
        let a = pos;
        let b = pos + vec2(w, 10.);
        let c = pos + vec2(w, h - 10.);
        let d = pos + vec2(0., h);
        draw_line(a.x, a.y, b.x, b.y, 1., BLACK);
        draw_line(b.x, b.y, c.x, c.y, 1., BLACK);
        draw_line(c.x, c.y, d.x, d.y, 1., BLACK);
        draw_line(d.x, d.y, a.x, a.y, 1., BLACK);
    }

    fn input_positions(&self) -> Vec<Vec2> {
        let mut input_pos = vec![vec2(self.size().x / 2., self.size().y - 5.)];
        let n_inputs = self.inputs.len();
        input_pos.extend(
            (0..n_inputs).map(|i| vec2(0., (i + 1) as f32 * self.size().y / (n_inputs + 1) as f32)),
        );
        input_pos
    }
}

impl Mux {
    fn n_inputs(&self) -> u8 {
        // FIXME: probably unnecessary due to Mux owning its inputs now
        // The first input is the select input
        1 << self.sel_bits
    }

    fn create_inputs(&self) -> Vec<Signal> {
        // first input is the select pin
        let mut inputs = vec![signal_zeros(1)];
        // then the actual input pins
        inputs.extend(vec![signal_zeros(self.data_bits); self.n_inputs() as usize]);
        inputs
    }
}

impl Default for Mux {
    fn default() -> Self {
        Self {
            sel_bits: 1,
            data_bits: 1,
            inputs: vec![signal_zeros(1); 2],
            output: signal_zeros(1),
            selector: signal_zeros(1),
        }
    }
}
#[derive(Debug, Clone)]
struct Demux {
    sel_bits: u8,
    data_bits: u8,
    input: Signal,
    outputs: Vec<Signal>,
    selector: Signal,
}

impl Logic for Demux {
    fn name(&self) -> &'static str {
        "Demultiplexer"
    }
    fn do_logic(&mut self) {
        let sel = self.selector.load::<usize>();
        for output in &mut self.outputs {
            output.fill(false);
        }
        self.outputs[sel].copy_from_bitslice(&self.input);
    }

    fn n_in_pins(&self) -> usize {
        2
    }

    fn n_out_pins(&self) -> usize {
        // Count selector as well
        self.outputs.len()
    }

    fn get_pin_value(&self, px: PinIndex) -> &Signal {
        match px {
            PinIndex::Input(i) => match i {
                0 => &self.selector,
                1 => &self.input,
                _ => panic!(),
            },
            PinIndex::Output(i) => &self.outputs[i],
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: &Signal) {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.selector.copy_from_bitslice(value),
                1 => self.input.copy_from_bitslice(value),
                _ => panic!(),
            },
            PinIndex::Output(i) => self.outputs[i].copy_from_bitslice(value),
        }
    }
}

impl Draw for Demux {
    fn size(&self) -> Vec2 {
        vec2(30., self.outputs.len() as f32 * 20.)
    }

    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();
        let a = pos + vec2(0., 10.);
        let b = pos + vec2(w, 0.);
        let c = pos + vec2(w, h);
        let d = pos + vec2(0., h - 10.);
        draw_line(a.x, a.y, b.x, b.y, 1., BLACK);
        draw_line(b.x, b.y, c.x, c.y, 1., BLACK);
        draw_line(c.x, c.y, d.x, d.y, 1., BLACK);
        draw_line(d.x, d.y, a.x, a.y, 1., BLACK);
    }

    fn input_positions(&self) -> Vec<Vec2> {
        vec![
            vec2(self.size().x / 2., self.size().y - 5.),
            vec2(0., self.size().y / 2.),
        ]
    }
}

impl Demux {
    fn n_outputs(&self) -> u8 {
        1 << self.sel_bits
    }

    fn create_inputs(&self) -> Vec<Signal> {
        // first input is the select pin
        // second input is the data
        vec![signal_zeros(self.sel_bits), signal_zeros(self.data_bits)]
    }

    fn create_outputs(&self) -> Vec<Signal> {
        vec![signal_zeros(self.data_bits); self.n_outputs() as usize]
    }
}
impl Default for Demux {
    fn default() -> Self {
        Self {
            sel_bits: 1,
            data_bits: 1,
            input: signal_zeros(1),
            outputs: vec![signal_zeros(1); 2],
            selector: signal_zeros(1),
        }
    }
}

#[derive(Debug, Clone)]
struct Register {
    data_bits: u8,
    write_enable: Signal,
    input: Signal,
    output: Signal,
}

impl Logic for Register {
    fn name(&self) -> &'static str {
        "Register"
    }
    fn do_logic(&mut self) {}
    fn is_clocked(&self) -> bool {
        true
    }
    fn tick_clock(&mut self) {
        if self.write_enable[0] {
            self.output.copy_from_bitslice(&self.input);
        }
    }

    fn n_in_pins(&self) -> usize {
        2
    }

    fn n_out_pins(&self) -> usize {
        1
    }

    fn get_pin_value(&self, px: PinIndex) -> &Signal {
        match px {
            PinIndex::Input(i) => match i {
                0 => &self.write_enable,
                1 => &self.input,
                _ => panic!(),
            },
            PinIndex::Output(i) => {
                if i == 0 {
                    &self.output
                } else {
                    panic!()
                }
            }
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: &Signal) {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.write_enable.copy_from_bitslice(value),
                1 => self.input.copy_from_bitslice(value),
                _ => panic!(),
            },
            PinIndex::Output(i) => {
                if i == 0 {
                    self.output.copy_from_bitslice(value);
                } else {
                    panic!()
                }
            }
        }
    }
}

impl Draw for Register {
    fn size(&self) -> Vec2 {
        Vec2::new(40., 60.)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();
        let in_color = if self.input.any() { GREEN } else { RED };
        draw_rectangle(pos.x, pos.y, w / 2., h, in_color);
        let out_color = if self.output.any() { GREEN } else { RED };
        draw_rectangle(pos.x + w / 2., pos.y, w / 2., h, out_color);
        draw_text("D", pos.x, pos.y + 25., 20., BLACK);
        draw_text("WE", pos.x, pos.y + 45., 20., BLACK);
        draw_text("Q", pos.x + 30., pos.y + 25., 20., BLACK);
    }
    fn input_positions(&self) -> Vec<Vec2> {
        vec![
            Vec2::new(0., 40.), // Write Enable
            Vec2::new(0., 20.),
        ]
    }
}

impl Default for Register {
    fn default() -> Self {
        Self {
            data_bits: 1,
            write_enable: signal_zeros(1),
            input: signal_zeros(1),
            output: signal_zeros(1),
        }
    }
}

#[derive(Debug, Clone)]
struct Input {
    data_bits: u8,
    value: Signal,
}

impl Logic for Input {
    fn name(&self) -> &'static str {
        "Input"
    }
    fn do_logic(&mut self) {}

    fn n_in_pins(&self) -> usize {
        0
    }

    fn n_out_pins(&self) -> usize {
        1
    }

    fn get_pin_value(&self, px: PinIndex) -> &Signal {
        match px {
            PinIndex::Input(_) => panic!(),
            PinIndex::Output(i) => {
                if i == 0 {
                    &self.value
                } else {
                    panic!()
                }
            }
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: &Signal) {
        match px {
            PinIndex::Input(_) => panic!(),
            PinIndex::Output(i) => {
                if i == 0 {
                    self.value.copy_from_bitslice(value)
                } else {
                    panic!()
                }
            }
        }
    }
    fn interact(&mut self) -> bool {
        if self.data_bits == 1 {
            let prev_value = self.value[0];
            self.value.set(0, !prev_value);
        } else {
            let prev_value = self.value.load::<u32>();
            self.value.copy_from_bitslice(
                &(prev_value + 1).view_bits::<Lsb0>()[..self.data_bits as usize],
            );
        }
        true
    }
}

impl Draw for Input {
    fn size(&self) -> Vec2 {
        Vec2::new(20., 20.)
    }

    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>) {
        let color = if self.value.any() { GREEN } else { RED };
        draw_rectangle(pos.x, pos.y, 20., 20., color);
    }
}

impl Default for Input {
    fn default() -> Self {
        Self {
            data_bits: 1,
            value: signal_zeros(1),
        }
    }
}

#[derive(Debug, Clone)]
struct Output {
    data_bits: u8,
    value: Signal,
}

impl Logic for Output {
    fn name(&self) -> &'static str {
        "Output"
    }
    fn do_logic(&mut self) {}

    fn n_in_pins(&self) -> usize {
        1
    }

    fn n_out_pins(&self) -> usize {
        0
    }

    fn get_pin_value(&self, px: PinIndex) -> &Signal {
        match px {
            PinIndex::Input(i) => {
                if i == 0 {
                    &self.value
                } else {
                    panic!()
                }
            }
            PinIndex::Output(_) => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: &Signal) {
        match px {
            PinIndex::Input(i) => {
                if i == 0 {
                    self.value.copy_from_bitslice(value)
                } else {
                    panic!()
                }
            }
            PinIndex::Output(_) => panic!(),
        }
    }
}

impl Draw for Output {
    fn size(&self) -> Vec2 {
        Vec2::new(20., 20.)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let color = if self.value.any() { GREEN } else { RED };
        draw_rectangle(pos.x, pos.y, 20., 20., color);
    }
}

impl Default for Output {
    fn default() -> Self {
        Self {
            data_bits: 1,
            value: signal_zeros(1),
        }
    }
}

trait Draw: Logic {
    fn size(&self) -> Vec2;
    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>);
    fn input_positions(&self) -> Vec<Vec2> {
        let n_inputs = self.n_in_pins();
        (0..n_inputs)
            .map(|i| vec2(0., (i + 1) as f32 * self.size().y / (n_inputs + 1) as f32))
            .collect()
    }
    fn output_positions(&self) -> Vec<Vec2> {
        let n_outputs = self.n_out_pins();
        (0..n_outputs)
            .map(|i| {
                vec2(
                    self.size().x,
                    (i + 1) as f32 * self.size().y / (n_outputs + 1) as f32,
                )
            })
            .collect()
    }
    fn draw_from_texture_slice(&self, pos: Vec2, tex: &Texture2D, tex_info: TexInfo) {
        draw_texture_ex(
            tex,
            pos.x,
            pos.y,
            WHITE,
            DrawTextureParams {
                dest_size: Some(tex_info.size / tex_info.scale),
                source: Some(Rect::new(
                    tex_info.offset.x,
                    tex_info.offset.y,
                    tex_info.size.x,
                    tex_info.size.y,
                )),
                rotation: 0.,
                flip_x: false,
                flip_y: false,
                pivot: None,
            },
        );
    }
}

trait Comp: Logic + Draw + Debug {}
impl<T: Logic + Draw + Debug> Comp for T {}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum PinIndex {
    Input(usize),
    Output(usize),
}

#[derive(Debug)]
struct Component {
    kind: Box<dyn Comp>,
    position: Vec2,
    // pins have a value and a position
    input_pos: Vec<Vec2>,
    output_pos: Vec<Vec2>,
}

impl Component {
    fn new(kind: Box<dyn Comp>, position: Vec2) -> Self {
        Self {
            position,
            input_pos: kind.input_positions(),
            output_pos: kind.output_positions(),
            kind,
        }
    }
    fn do_logic(&mut self) {
        self.kind.do_logic();
    }
    fn clock_update(&mut self) {
        // TODO: make is_clocked part of the type structure
        if self.kind.is_clocked() {
            self.kind.tick_clock();
        }
    }
}

struct TexInfo {
    offset: Vec2,
    size: Vec2,
    scale: f32,
}

impl TexInfo {
    fn new(offset: Vec2, size: Vec2, scale: f32) -> Self {
        Self {
            offset,
            size,
            scale,
        }
    }
}

#[derive(Debug)]
struct Wire {
    // TODO: does Wire even need data_bits? Can just use Signal::len
    start_comp: NodeIndex,
    start_pin: usize,
    end_comp: NodeIndex,
    end_pin: usize,
    data_bits: u8,
    value: Signal,
}

impl Wire {
    fn new(
        start_comp: NodeIndex,
        start_pin: usize,
        end_comp: NodeIndex,
        end_pin: usize,
        data_bits: u8,
    ) -> Self {
        Self {
            start_comp,
            start_pin,
            end_comp,
            end_pin,
            data_bits,
            value: signal_zeros(data_bits),
        }
    }
}

#[derive(Default, Debug, Clone, Copy)]
enum ActionState {
    #[default]
    Idle,
    // A new component from the menu that has been temporarily added to the graph
    HoldingComponent(NodeIndex),
    // Left-clicked on a component in the sandbox area
    SelectingComponent(NodeIndex),
    // Moving a component that already was in the sandbox area
    MovingComponent(NodeIndex),
    DrawingWire(NodeIndex, PinIndex),
}

#[derive(Default, Debug)]
struct App {
    textures: HashMap<&'static str, Texture2D>,
    graph: StableGraph<Component, Wire>,
    action_state: ActionState,
}

impl App {
    async fn new() -> Self {
        App {
            textures: Self::load_textures().await,
            ..Default::default()
        }
    }
    async fn load_textures() -> HashMap<&'static str, Texture2D> {
        HashMap::from([(
            "gates",
            load_texture("assets/logic_gates.png").await.unwrap(),
        )])
    }

    fn draw_all_components(&self) {
        for comp in self.graph.node_weights() {
            comp.kind.draw(comp.position, &self.textures);
        }
    }

    fn draw_selected_component_box(&self, cx: NodeIndex) {
        let comp = &self.graph[cx];
        let (w, h) = comp.kind.size().into();
        draw_rectangle_lines(comp.position.x, comp.position.y, w, h, 2., BLACK);
    }

    fn draw_all_wires(&self) {
        for wire in self.graph.edge_weights() {
            self.draw_wire(wire);
        }
    }
    fn draw_wire(&self, wire: &Wire) {
        let cx_a = &self.graph[wire.start_comp];
        let cx_b = &self.graph[wire.end_comp];
        let pos_a = cx_a.position + cx_a.output_pos[wire.start_pin];
        let pos_b = cx_b.position + cx_b.input_pos[wire.end_pin];
        let color = if wire.value.any() { GREEN } else { BLUE };
        let thickness = if wire.data_bits == 1 { 1. } else { 2. };
        draw_ortho_lines(pos_a, pos_b, color, thickness);
    }

    fn select_component(&mut self, cx: NodeIndex) {
        let comp = &mut self.graph[cx];
        if comp.kind.interact() {
            self.update_signals();
        }
    }
    fn draw_pin_highlight(&self, cx: NodeIndex, px: PinIndex) {
        let comp = &self.graph[cx];
        let pin_pos = match px {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        // find absolute pin_pos (it is relative position out of the box)
        let pin_pos = comp.position + pin_pos;
        draw_circle_lines(pin_pos.x, pin_pos.y, 5., 1., GREEN);
    }

    fn find_hovered_cx_and_pin(&self) -> Option<(NodeIndex, Option<PinIndex>)> {
        // Looks for a hovered component, and then for a hovered pin if a component is found.
        let comp = self.find_hovered_comp()?;
        let pin = self.find_hovered_pin(comp);
        Some((comp, pin))
    }

    fn find_hovered_comp(&self) -> Option<NodeIndex> {
        let (mx, my) = mouse_position();

        for cx in self.graph.node_indices() {
            let comp = &self.graph[cx];
            let (x, y) = comp.position.into();
            let (w, h) = comp.kind.size().into();
            if mx >= x && mx <= x + w && my >= y && my <= y + h {
                return Some(cx);
            }
        }
        None
    }

    fn find_hovered_pin(&self, cx: NodeIndex) -> Option<PinIndex> {
        let mouse_pos = Vec2::from(mouse_position());

        let comp = &self.graph[cx];
        let max_dist = 10.;

        for (i, pin_pos) in comp.input_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < max_dist {
                return Some(PinIndex::Input(i));
            }
        }
        for (i, pin_pos) in comp.output_pos.iter().enumerate() {
            let pin_pos = vec2(comp.position.x + pin_pos.x, comp.position.y + pin_pos.y);
            if mouse_pos.distance(pin_pos) < max_dist {
                return Some(PinIndex::Output(i));
            }
        }
        None
    }
    fn try_add_wire(
        &mut self,
        cx_a: NodeIndex,
        px_a: PinIndex,
        cx_b: NodeIndex,
        px_b: PinIndex,
    ) -> bool {
        // Do not allow wires within a single component
        if cx_a == cx_b {
            return false;
        }
        // determine which pin is the output (sender) and which is the input (receiver)
        let (cx_a, pin_a, cx_b, pin_b) = match (px_a, px_b) {
            (PinIndex::Output(pin_a), PinIndex::Input(pin_b)) => (cx_a, pin_a, cx_b, pin_b),
            (PinIndex::Input(pin_a), PinIndex::Output(pin_b)) => (cx_b, pin_b, cx_a, pin_a),
            // input->input or output->output are invalid connections; don't create the wire.
            _ => return false,
        };
        // Check that the input pin is not already occupied
        if self
            .graph
            .edges_directed(cx_b, Incoming)
            .any(|e| e.weight().end_pin == pin_b)
        {
            return false;
        }
        // FIXME: Figure out how to determine the number of data_bits based on start_comp and
        // end_comp
        let wire = Wire::new(cx_a, pin_a, cx_b, pin_b, 1);
        self.graph.add_edge(cx_a, cx_b, wire);
        self.update_signals();
        true
    }

    fn remove_component(&mut self, cx: NodeIndex) {
        self.graph.remove_node(cx);
    }

    fn tick_clock(&mut self) {
        for comp in self.graph.node_weights_mut() {
            comp.clock_update();
        }
        self.update_signals();
    }

    fn update_signals(&mut self) {
        // Remove (valid) cycles by ignoring edges which lead into a clocked component.
        let de_cycled =
            EdgeFiltered::from_fn(&self.graph, |e| !self.graph[e.target()].kind.is_clocked());
        let order =
            toposort(&de_cycled, None).expect("Cycles should only involve clocked components");

        // step through all components in order of evaluation
        for cx in order {
            // When visiting a component, perform logic to convert inputs to outputs.
            // This also applies to clocked components, whose inputs will still be based on the previous clock cycle.
            self.graph[cx].do_logic();
            let mut edges = self.graph.neighbors(cx).detach();
            // step through all connected wires and their corresponding components
            while let Some((wx, next_node_idx)) = edges.next(&self.graph) {
                let start_pin = PinIndex::Output(self.graph[wx].start_pin);
                let end_pin = PinIndex::Input(self.graph[wx].end_pin);
                // use wire to determine relevant output and input pins
                // TODO: maybe rework the graph so that edges are between the pins, not the components?
                let signal_to_transmit = self.graph[cx].kind.get_pin_value(start_pin).clone();
                self.graph[next_node_idx]
                    .kind
                    .set_pin_value(end_pin, &signal_to_transmit);
                self.graph[wx].value.copy_from_bitslice(&signal_to_transmit);
            }
        }
    }

    fn draw_temp_wire(&self, cx: NodeIndex, px: PinIndex) {
        let comp = &self.graph[cx];
        let pin_pos = match px {
            PinIndex::Input(i) => comp.input_pos[i],
            PinIndex::Output(i) => comp.output_pos[i],
        };
        let start_pos = comp.position + pin_pos;
        let end_pos = Vec2::from(mouse_position());

        draw_ortho_lines(start_pos, end_pos, BLACK, 1.);
    }

    fn get_properties_ui(&mut self, prop_ui: &mut Ui) {
        if let ActionState::SelectingComponent(cx) = self.action_state {
            let comp = &self.graph[cx];
            Group::new(hash!(), vec2(MENU_SIZE.x, 30.))
                .position(Vec2::ZERO)
                .ui(prop_ui, |ui| {
                    ui.label(vec2(0., 0.), "ID");
                    ui.label(vec2(50., 0.), comp.kind.name());
                });
            // Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(prop_ui, |ui| {
            //     ui.label(vec2(0., 0.), "Value:");
            //     let value = match comp.kind {
            //         CompKind::Output { .. } => comp.inputs[0].load::<u32>(),
            //         _ => comp.outputs[0].load::<u32>(),
            //     };
            //     ui.label(vec2(50., 0.), &format!("{}", value));
            // });
            // TODO: Make custom property ui fields
        }
    }

    // fn sel_bits_ui(&self, comp_kind: &CompKind, ui: &mut Ui) {
    //     let sel_bits = match comp_kind {
    //         CompKind::Mux(mux) => mux.sel_bits,
    //         CompKind::Demux(demux) => demux.sel_bits,
    //         _ => return, // No sel_bits on comp, so don't render the config option
    //     };
    //
    //     Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
    //         let new_sel_bits =
    //             (ui.combo_box(hash!(), "Select Bits", &["1", "2", "3", "4", "5"], None) + 1) as u8;
    //         // TODO: Actually update the component
    //
    //         if sel_bits == new_sel_bits {
    //             return;
    //         }
    //         let new_comp_kind = match comp_kind {
    //             CompKind::Mux(mux) => CompKind::Mux(Mux {
    //                 sel_bits: new_sel_bits,
    //                 data_bits: mux.data_bits,
    //             }),
    //             CompKind::Demux(demux) => CompKind::Demux(Demux {
    //                 sel_bits: new_sel_bits,
    //                 data_bits: demux.data_bits,
    //             }),
    //             _ => unreachable!("Only components with select bits possible"),
    //         };
    //     });
    // }

    // draw wire so that it only travels orthogonally
    fn update(&mut self, selected_menu_comp_name: &mut Option<&str>) {
        let mouse_pos = Vec2::from(mouse_position());
        let hover_result = self.find_hovered_cx_and_pin();
        if in_sandbox_area(mouse_pos) {
            // Alternatively could remove ActionState to use its value without mutating App.
            // let prev_state = std::mem::take(&mut self.action_state);

            // Clone the current ActionState to allow mutation
            let prev_state = self.action_state;
            // Return the new ActionState from the match. This makes it hard to mess up.
            self.action_state = match prev_state {
                ActionState::Idle => 'idle: {
                    match hover_result {
                        // Hovering  on component, but NOT pin
                        Some((cx, None)) => {
                            if is_mouse_button_pressed(MouseButton::Left) {
                                self.select_component(cx);
                                break 'idle ActionState::SelectingComponent(cx);
                            }
                        }
                        // Hovering on pin
                        Some((cx, Some(px))) => {
                            if is_mouse_button_pressed(MouseButton::Left) {
                                break 'idle ActionState::DrawingWire(cx, px);
                            }
                        }
                        // Not hovering anything
                        None => break 'idle ActionState::Idle,
                    }
                    ActionState::Idle
                }
                ActionState::HoldingComponent(cx) => {
                    self.graph[cx].position = mouse_pos - self.graph[cx].kind.size() / 2.;

                    if is_mouse_button_released(MouseButton::Left) {
                        // component is completely added to sandbox, so get rid of menu selection.
                        *selected_menu_comp_name = None;
                        ActionState::Idle
                    } else if is_mouse_button_released(MouseButton::Right)
                        || is_key_released(KeyCode::Escape)
                    {
                        // Remove temporary component from graph
                        self.graph.remove_node(cx);
                        ActionState::Idle
                    } else {
                        prev_state
                    }
                }
                ActionState::SelectingComponent(cx) => {
                    // `D` deletes the component
                    if is_key_released(KeyCode::D) {
                        self.remove_component(cx);
                        ActionState::Idle
                    // `Esc` de-selects the component
                    } else if is_key_released(KeyCode::Escape) {
                        ActionState::Idle
                    // Clicking either de-selects the component, selects a new component, or begins drawing a wire
                    } else if is_mouse_button_pressed(MouseButton::Left) {
                        match hover_result {
                            // Hovering  on component, but NOT pin
                            Some((new_cx, None)) => {
                                self.select_component(new_cx);
                                ActionState::SelectingComponent(new_cx)
                            }
                            // Hovering on pin
                            Some((new_cx, Some(new_px))) => {
                                ActionState::DrawingWire(new_cx, new_px)
                            }
                            // Not hovering anything
                            None => ActionState::Idle,
                        }
                    // If mouse is dragging, switch from selecting to moving component.
                    } else if is_mouse_button_down(MouseButton::Left)
                        && mouse_delta_position() != Vec2::ZERO
                    {
                        ActionState::MovingComponent(cx)
                    } else {
                        ActionState::SelectingComponent(cx)
                    }
                }
                ActionState::MovingComponent(cx) => {
                    // Update component position (and center on mouse)
                    self.graph[cx].position = mouse_pos - self.graph[cx].kind.size() / 2.;
                    if is_mouse_button_released(MouseButton::Left) {
                        ActionState::SelectingComponent(cx)
                    } else {
                        prev_state
                    }
                }
                ActionState::DrawingWire(start_cx, start_px) => {
                    // Potentially finalizing the wire
                    if is_mouse_button_released(MouseButton::Left) {
                        // Landed on a pin
                        if let Some((end_cx, Some(end_px))) = hover_result {
                            // This function handles all error cases like bad pin match-up, self-connection, and multiple output connections
                            self.try_add_wire(start_cx, start_px, end_cx, end_px);
                        }
                        ActionState::Idle
                    // In the process of drawing the wire
                    } else if is_mouse_button_down(MouseButton::Left) {
                        self.draw_temp_wire(start_cx, start_px);
                        ActionState::DrawingWire(start_cx, start_px)
                    // Let go of wire without completing it
                    } else {
                        ActionState::Idle
                    }
                }
            };
        }
        // Tick clock on spacebar
        if is_key_pressed(KeyCode::Space) {
            self.tick_clock();
        }

        // Do all drawing at the end to make sure everything is updated
        // and so that the z-order is maintained.
        self.draw_all_components();
        self.draw_all_wires();
        if let Some((cx, Some(px))) = self.find_hovered_cx_and_pin() {
            self.draw_pin_highlight(cx, px);
        }
        if let ActionState::SelectingComponent(cx) = self.action_state {
            self.draw_selected_component_box(cx);
        }
    }
}

const SANDBOX_POS: Vec2 = vec2(210., 0.);
const SANDBOX_SIZE: Vec2 = vec2(600., 600.);
const WINDOW_SIZE: Vec2 = vec2(1000., 600.);
const MENU_SIZE: Vec2 = vec2(200., WINDOW_SIZE.y);

fn create_skin() -> Skin {
    let window_style = root_ui()
        .style_builder()
        .background(Image {
            width: 3,
            height: 3,
            bytes: vec![
                68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255, 238, 238, 238,
                255, 68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255, 68, 68, 68, 255,
            ],
        })
        .color_inactive(Color::from_rgba(255, 255, 255, 255))
        .background_margin(RectOffset::new(1., 1., 1., 1.))
        .build();
    Skin {
        window_style,
        ..root_ui().default_skin()
    }
}

#[macroquad::main("Logisim")]
async fn main() {
    let mut app = App::new().await;
    let skin = create_skin();
    root_ui().push_skin(&skin);

    let folder_structure = get_folder_structure();
    let mut selected_menu_comp_name = None;

    loop {
        clear_background(GRAY);
        set_default_camera();
        // Draw Left-Side Menu
        Window::new(hash!("left-menu"), Vec2::ZERO, MENU_SIZE)
            .label("Components")
            .titlebar(true)
            .movable(false)
            .ui(&mut root_ui(), |ui| {
                // Draw Components menu
                Group::new(hash!("components"), vec2(MENU_SIZE.x, MENU_SIZE.y / 2.))
                    .position(Vec2::ZERO)
                    .ui(ui, |ui| {
                        for (folder, comp_names) in &folder_structure {
                            TreeNode::new(hash!(*folder), *folder)
                            .init_unfolded()
                            .ui(ui, |ui| {
                                for &comp_name in comp_names {
                                    if Button::new(comp_name)
                                        .selected(selected_menu_comp_name == Some(comp_name))
                                        .ui(ui)
                                    {
                                        // track selection in menu UI
                                        selected_menu_comp_name = Some(comp_name);
                                        // create component for App
                                        let kind: Box<dyn Comp> = match comp_name {
                                            "NOT" => Box::new(Gate::default_of_kind(GateKind::Not)),
                                            "AND" => Box::new(Gate::default_of_kind(GateKind::And)),
                                            "OR" => Box::new(Gate::default_of_kind(GateKind::Or)),
                                            "Input" => Box::new(Input::default()),
                                            "Output" => Box::new( Output::default()),
                                            "Register" => Box::new(Register::default()),
                                            "Mux" => Box::new(Mux::default()),
                                            "Demux" => Box::new(Demux::default()), 
                                            _ => {
                                                panic!("Unknown component attempted to be created.")
                                            }
                                        };
                                        let new_comp = Component::new(kind, Vec2::ZERO);
                                        let new_cx = app.graph.add_node(new_comp);
                                        app.action_state = ActionState::HoldingComponent(new_cx);
                                    };
                                }
                            });
                        }
                    });
                Group::new(hash!("properties"), vec2(MENU_SIZE.x, MENU_SIZE.y / 2.))
                    .position(vec2(0., MENU_SIZE.y / 2.))
                    .ui(ui, |ui| {
                        app.get_properties_ui(ui);
                    });
            });
        // Draw circuit sandbox area
        draw_rectangle(
            SANDBOX_POS.x,
            SANDBOX_POS.y,
            SANDBOX_SIZE.x,
            SANDBOX_POS.y,
            GRAY,
        );
        // Draw in sandbox area
        app.update(&mut selected_menu_comp_name);

        next_frame().await;
    }
}

fn in_sandbox_area(pos: Vec2) -> bool {
    let sandbox_rect = Rect::new(SANDBOX_POS.x, SANDBOX_POS.y, SANDBOX_SIZE.x, SANDBOX_SIZE.y);
    sandbox_rect.contains(pos)
}

fn get_folder_structure() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("Gates", vec!["NOT", "AND", "OR"]),
        ("I/O", vec!["Input", "Output"]),
        ("Plexers", vec!["Mux", "Demux"]),
        ("Memory", vec!["Register"]),
    ]
}

fn draw_ortho_lines(start: Vec2, end: Vec2, color: Color, thickness: f32) {
    // TODO: make this more sophisticated so that it chooses the right order (horiz/vert first)
    draw_line(start.x, start.y, end.x, start.y, 1., color);
    draw_line(end.x, start.y, end.x, end.y, thickness, color);
}

use bitvec::prelude::*;
use macroquad::prelude::*;
use macroquad::ui::widgets::Group;
use macroquad::ui::{hash, Ui};
use std::collections::HashMap;

use std::fmt::Debug;

use crate::MENU_SIZE;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PinIndex {
    Input(usize),
    Output(usize),
}

#[derive(Debug)]
pub struct Component {
    pub(crate) kind: Box<dyn Comp>,
    pub(crate) position: Vec2,
    bboxes: Vec<Rect>,
    pub(crate) input_pos: Vec<Vec2>,
    pub(crate) output_pos: Vec<Vec2>,
}

impl Component {
    pub(crate) fn new(kind: Box<dyn Comp>, position: Vec2) -> Self {
        Self {
            position,
            input_pos: kind.input_positions(),
            output_pos: kind.output_positions(),
            bboxes: kind.bboxes(),
            kind,
        }
    }
    pub(crate) fn contains(&self, point: Vec2) -> bool {
        for bbox in &self.bboxes {
            if bbox.offset(self.position).contains(point) {
                return true;
            }
        }
        false
    }
    pub(crate) fn do_logic(&mut self) {
        self.kind.do_logic();
    }
    pub(crate) fn draw(&self, textures: &HashMap<&str, Texture2D>) {
        self.kind.draw(self.position, textures)
    }
    pub(crate) fn clock_update(&mut self) {
        // TODO: make is_clocked part of the type structure
        if self.kind.is_clocked() {
            self.kind.tick_clock();
        }
    }
    pub(crate) fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        self.kind.draw_properties_ui(ui)
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

pub type Signal = BitVec<u32, Lsb0>;

pub fn signal_zeros(n: u8) -> Signal {
    bitvec![u32, Lsb0; 0; n as usize]
}

pub(crate) trait Logic {
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

#[derive(Debug, Clone, Copy)]
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

const COMBO_OPTS: &[&str] = &[
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17",
    "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31", "32",
];

impl Draw for Gate {
    fn size(&self) -> Vec2 {
        let tex_info = self.tex_info();
        tex_info.size / tex_info.scale
    }

    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>) {
        self.draw_from_texture_slice(pos, textures.get("gates").unwrap(), self.tex_info());
        if self.n_inputs > 2 {
            let y_offset = (self.n_inputs as f32 - 1.) / 2. * 20.;
            draw_line(
                pos.x,
                pos.y + self.size().y / 2. - y_offset,
                pos.x,
                pos.y + self.size().y / 2. + y_offset,
                2.,
                BLACK,
            )
        }
    }
    fn bboxes(&self) -> Vec<Rect> {
        let mut bboxes = vec![Rect::new(0., 0., self.size().x, self.size().y)];
        if self.n_inputs > 2 {
            let y_offset = (self.n_inputs as f32 - 1.) / 2. * 20.;
            bboxes.push(Rect::new(
                -5.,
                -y_offset + self.size().y / 2.,
                5.,
                2. * y_offset,
            ));
        }
        bboxes
    }
    fn input_positions(&self) -> Vec<Vec2> {
        if self.n_inputs <= 2 {
            match self.kind {
                GateKind::Not => vec![vec2(0., 20.)],
                GateKind::Or => vec![vec2(0., 10.), vec2(0., 28.)],
                GateKind::And => vec![vec2(0., 8.), vec2(0., 25.)],
            }
        } else {
            let y_offset = (self.n_inputs as f32 - 1.) / 2. * 20.;
            (0..self.n_inputs)
                .map(|i| vec2(0., self.size().y / 2. - y_offset + 20. * i as f32))
                .collect()
        }
    }
    fn output_positions(&self) -> Vec<Vec2> {
        let tex_info = self.tex_info();
        vec![vec2(tex_info.size.x, tex_info.size.y / 2.) / tex_info.scale]
    }
    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        let mut new_comp: Option<Box<dyn Comp>> = None;
        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Data bits
            let mut data_bits_sel = self.data_bits as usize - 1;
            ui.combo_box(hash!(), "Data Bits:", COMBO_OPTS, &mut data_bits_sel);
            let new_data_bits = data_bits_sel as u8 + 1;

            if new_data_bits != self.data_bits {
                let gate = Self::new(self.kind, new_data_bits, self.n_inputs);
                new_comp = Some(Box::new(gate));
            };
        });
        if !matches!(self.kind, GateKind::Not) {
            // Number of inputs
            Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
                let mut n_inputs_sel = self.n_inputs - 2;
                ui.combo_box(hash!(), "Inputs:", &COMBO_OPTS[1..11], &mut n_inputs_sel);
                let new_n_inputs = n_inputs_sel + 2;
                if new_n_inputs != self.n_inputs {
                    let gate = Self::new(self.kind, self.data_bits, new_n_inputs);
                    new_comp = Some(Box::new(gate));
                }
            });
        }
        new_comp
    }
}

impl Gate {
    fn new(kind: GateKind, data_bits: u8, n_inputs: usize) -> Self {
        Self {
            kind,
            data_bits,
            n_inputs,
            inputs: vec![signal_zeros(data_bits); n_inputs],
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
    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        let mut new_comp: Option<Box<dyn Comp>> = None;
        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Data bits
            let mut data_bits_sel = self.data_bits as usize - 1;
            ui.combo_box(hash!(), "Data Bits:", COMBO_OPTS, &mut data_bits_sel);
            let new_data_bits = data_bits_sel as u8 + 1;

            if new_data_bits != self.data_bits {
                let mux = Self::new(self.sel_bits, new_data_bits);
                new_comp = Some(Box::new(mux));
            };
        });

        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Selection bits
            let mut sel_bits_sel = self.sel_bits as usize - 1;
            ui.combo_box(hash!(), "Select Bits:", &COMBO_OPTS[..6], &mut sel_bits_sel);
            let new_sel_bits = sel_bits_sel as u8 + 1;
            if new_sel_bits != self.sel_bits {
                let mux = Self::new(new_sel_bits, self.data_bits);
                new_comp = Some(Box::new(mux));
            }
        });

        new_comp
    }
}

impl Mux {
    fn new(sel_bits: u8, data_bits: u8) -> Self {
        Self {
            sel_bits,
            data_bits,
            inputs: vec![signal_zeros(data_bits); 1 << sel_bits],
            output: signal_zeros(data_bits),
            selector: signal_zeros(sel_bits),
        }
    }
    fn n_inputs(&self) -> u8 {
        1 << self.sel_bits
    }
}

impl Default for Mux {
    fn default() -> Self {
        Self::new(1, 1)
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

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        let mut new_comp: Option<Box<dyn Comp>> = None;
        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Data bits
            let mut data_bits_sel = self.data_bits as usize - 1;
            ui.combo_box(hash!(), "Data Bits:", COMBO_OPTS, &mut data_bits_sel);
            let new_data_bits = data_bits_sel as u8 + 1;

            if new_data_bits != self.data_bits {
                let demux = Self::new(self.sel_bits, new_data_bits);
                new_comp = Some(Box::new(demux));
            };
        });

        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Selection bits
            let mut sel_bits_sel = self.sel_bits as usize - 1;
            ui.combo_box(
                hash!(),
                "Select Bits:",
                &["1", "2", "3", "4", "5", "6"],
                &mut sel_bits_sel,
            );
            let new_sel_bits = sel_bits_sel as u8 + 1;
            if new_sel_bits != self.sel_bits {
                let mux = Self::new(new_sel_bits, self.data_bits);
                new_comp = Some(Box::new(mux));
            }
        });

        new_comp
    }
}

impl Demux {
    fn new(sel_bits: u8, data_bits: u8) -> Self {
        Self {
            sel_bits,
            data_bits,
            input: signal_zeros(data_bits),
            outputs: vec![signal_zeros(data_bits); 1 << sel_bits],
            selector: signal_zeros(sel_bits),
        }
    }
}

impl Default for Demux {
    fn default() -> Self {
        Self::new(1, 1)
    }
}

#[derive(Debug, Clone)]
struct Register {
    data_bits: u8,
    write_enable: Signal,
    input: Signal,
    output: Signal,
}

impl Register {
    fn new(data_bits: u8) -> Self {
        Self {
            data_bits,
            write_enable: signal_zeros(1),
            input: signal_zeros(data_bits),
            output: signal_zeros(data_bits),
        }
    }
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

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        let mut new_comp: Option<Box<dyn Comp>> = None;
        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Data bits
            let mut data_bits_sel = self.data_bits as usize - 1;
            ui.combo_box(hash!(), "Data Bits:", COMBO_OPTS, &mut data_bits_sel);
            let new_data_bits = data_bits_sel as u8 + 1;

            if new_data_bits != self.data_bits {
                let reg = Self::new(new_data_bits);
                new_comp = Some(Box::new(reg));
            };
        });
        new_comp
    }
}

impl Default for Register {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Debug, Clone)]
struct Input {
    data_bits: u8,
    value: Signal,
}

impl Input {
    fn new(data_bits: u8) -> Self {
        Self {
            data_bits,
            value: signal_zeros(data_bits),
        }
    }
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

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        let mut new_comp: Option<Box<dyn Comp>> = None;
        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Data bits
            let mut data_bits_sel = self.data_bits as usize - 1;
            ui.combo_box(hash!(), "Data Bits:", COMBO_OPTS, &mut data_bits_sel);
            let new_data_bits = data_bits_sel as u8 + 1;

            if new_data_bits != self.data_bits {
                let input = Self::new(new_data_bits);
                new_comp = Some(Box::new(input));
            };
        });

        new_comp
    }
}

impl Default for Input {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Debug, Clone)]
struct Output {
    data_bits: u8,
    value: Signal,
}

impl Output {
    fn new(data_bits: u8) -> Self {
        Self {
            data_bits,
            value: signal_zeros(data_bits),
        }
    }
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

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        let mut new_comp: Option<Box<dyn Comp>> = None;
        Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
            // Data bits
            let mut data_bits_sel = self.data_bits as usize - 1;
            ui.combo_box(hash!(), "Data Bits:", COMBO_OPTS, &mut data_bits_sel);
            let new_data_bits = data_bits_sel as u8 + 1;

            if new_data_bits != self.data_bits {
                let output = Self::new(new_data_bits);
                new_comp = Some(Box::new(output));
            };
        });

        new_comp
    }
}

impl Default for Output {
    fn default() -> Self {
        Self::new(1)
    }
}

pub(crate) trait Draw: Logic {
    fn size(&self) -> Vec2;
    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>);
    fn bboxes(&self) -> Vec<Rect> {
        // Return bounding boxes for this component, located relative to its position
        vec![Rect::new(0., 0., self.size().x, self.size().y)]
    }
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
    fn draw_properties_ui(&mut self, ui: &mut Ui) -> Option<Box<dyn Comp>> {
        // FIXME: Remove default impl when caught up.
        None
    }
}

pub(crate) trait Comp: Logic + Draw + Debug {}
impl<T: Logic + Draw + Debug> Comp for T {}

pub(crate) fn default_comp_from_name(comp_name: &str) -> Component {
    let kind: Box<dyn Comp> = match comp_name {
        "NOT" => Box::new(Gate::default_of_kind(GateKind::Not)),
        "AND" => Box::new(Gate::default_of_kind(GateKind::And)),
        "OR" => Box::new(Gate::default_of_kind(GateKind::Or)),
        "Input" => Box::new(Input::default()),
        "Output" => Box::new(Output::default()),
        "Register" => Box::new(Register::default()),
        "Mux" => Box::new(Mux::default()),
        "Demux" => Box::new(Demux::default()),
        _ => {
            panic!("Unknown component attempted to be created.")
        }
    };

    Component::new(kind, Vec2::ZERO)
}

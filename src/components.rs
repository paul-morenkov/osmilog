use bitvec::prelude::*;
use egui_macroquad::{
    egui::{ComboBox, Ui},
    macroquad,
};
use macroquad::prelude::*;
use std::{collections::HashMap, fmt::Display};

use crate::{CtxEvent, TunnelUpdate, TunnelUpdateKind};

use std::fmt::Debug;

use crate::TILE_SIZE;

const COMBO_WIDTH: f32 = 50.;
const PIN_RADIUS: f32 = 2.;

pub type Signal = BitVec<u32, Lsb0>;
pub type SignalRef<'a> = &'a BitSlice<u32, Lsb0>;

#[derive(Debug, Clone)]
struct Pin {
    bits: u8,
    signal: Option<Signal>,
}

impl Pin {
    fn new(bits: u8) -> Self {
        Self { bits, signal: None }
    }

    fn get(&self) -> Option<SignalRef> {
        self.signal.as_deref()
    }
    fn set(&mut self, value: Option<SignalRef>) {
        match value {
            None => self.signal = None,
            Some(new) => match &mut self.signal {
                Some(old) => old.copy_from_bitslice(new),
                None => self.signal = Some(Signal::from_bitslice(new)),
            },
        }
    }

    fn color(&self) -> Color {
        color_from_signal(self.signal.as_deref())
    }
}

impl Default for Pin {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PinIndex {
    Input(usize),
    Output(usize),
}

// None -> component didn't change
// Some(None) -> component changed, but doesn't require context update
// Some(Some(event)) -> component changed AND requires context update
pub(crate) type CompUpdateResponse = Option<Option<CtxEvent>>;

#[derive(Debug)]
pub enum CompEvent {
    Added,
    Removed,
}

#[derive(Debug)]
pub struct Component {
    pub(crate) kind: Box<dyn Comp>,
    pub(crate) position: Vec2,
    pub(crate) input_pos: Vec<Vec2>,
    pub(crate) output_pos: Vec<Vec2>,
    bboxes: Vec<Rect>,
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
        self.kind.draw(self.position, textures);
        self.draw_pins();
    }

    fn draw_pins(&self) {
        let (x, y) = self.position.into();
        for i in 0..self.kind.n_in_pins() {
            let color = self.kind.color_from_px(PinIndex::Input(i));
            let pin_pos = self.input_pos[i];
            draw_circle(x + pin_pos.x, y + pin_pos.y, PIN_RADIUS, color);
        }

        for i in 0..self.kind.n_out_pins() {
            let color = self.kind.color_from_px(PinIndex::Output(i));
            let pin_pos = self.output_pos[i];
            draw_circle(x + pin_pos.x, y + pin_pos.y, PIN_RADIUS, color);
        }
    }

    pub(crate) fn clock_update(&mut self) {
        if self.kind.is_clocked() {
            self.kind.tick_clock();
        }
    }
    pub(crate) fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        self.kind.draw_properties_ui(ui)
    }
}

pub(crate) struct TexInfo {
    offset: Vec2,
    tex_size: Vec2,
    size: Vec2,
}

impl TexInfo {
    fn new(offset: Vec2, tex_size: Vec2, size: Vec2) -> Self {
        Self {
            offset,
            tex_size,
            size,
        }
    }
}

pub fn signal_zeros(n: u8) -> Signal {
    bitvec![u32, Lsb0; 0; n as usize]
}

pub(crate) trait Logic {
    fn name(&self) -> &'static str;
    fn n_in_pins(&self) -> usize;
    fn n_out_pins(&self) -> usize;
    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef>;
    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>);
    fn get_pin_width(&self, px: PinIndex) -> u8;
    fn do_logic(&mut self);
    fn is_clocked(&self) -> bool {
        false
    }
    fn tick_clock(&mut self) {}
    // Any additional changes that happen when you click on the component
    fn interact(&mut self) -> bool {
        false
    }
    fn get_ctx_event(&mut self, _: CompEvent) -> Option<CtxEvent> {
        None
    }
}

pub(crate) trait Draw: Logic {
    fn size(&self) -> Vec2;
    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>);
    fn bboxes(&self) -> Vec<Rect> {
        // Return bounding boxes for this component, located relative to its position
        vec![Rect::new(
            -TILE_SIZE,
            -TILE_SIZE,
            self.size().x + 2. * TILE_SIZE,
            self.size().y + 2. * TILE_SIZE,
        )]
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
            *tex,
            pos.x,
            pos.y,
            WHITE,
            DrawTextureParams {
                dest_size: Some(tex_info.size),
                source: Some(Rect::new(
                    tex_info.offset.x,
                    tex_info.offset.y,
                    tex_info.tex_size.x,
                    tex_info.tex_size.y,
                )),
                rotation: 0.,
                flip_x: false,
                flip_y: false,
                pivot: None,
            },
        );
    }

    fn color_from_px(&self, px: PinIndex) -> Color {
        color_from_signal(self.get_pin_value(px))
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse;
}

pub(crate) trait Comp: Logic + Draw + Debug {}
impl<T: Logic + Draw + Debug> Comp for T {}

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
    inputs: Vec<Pin>,
    output: Pin,
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

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        match px {
            PinIndex::Input(i) => self.inputs[i].get(),
            PinIndex::Output(0) => self.output.get(),
            _ => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        match px {
            PinIndex::Input(i) => {
                self.inputs[i].set(value);
            }
            PinIndex::Output(0) => self.output.set(value),
            _ => panic!(),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        match px {
            PinIndex::Input(i) => self.inputs[i].bits,
            PinIndex::Output(0) => self.output.bits,
            _ => panic!(),
        }
    }

    fn do_logic(&mut self) {
        self.output.signal = match self.kind {
            GateKind::Not => self.inputs[0].signal.clone().map(|s| !s),
            GateKind::Or => 'a: {
                let mut result = signal_zeros(self.data_bits);
                for input in &self.inputs {
                    match &input.signal {
                        Some(s) => result |= s,
                        None => {
                            break 'a None;
                        }
                    }
                }
                Some(result)
            }
            GateKind::And => 'a: {
                let mut result = !signal_zeros(self.data_bits);
                for input in &self.inputs {
                    match &input.signal {
                        Some(s) => result &= s,
                        None => break 'a None,
                    }
                }
                Some(result)
            }
        };
    }
}

impl Draw for Gate {
    fn size(&self) -> Vec2 {
        self.tex_info().size
    }

    fn draw(&self, pos: Vec2, textures: &HashMap<&str, Texture2D>) {
        self.draw_from_texture_slice(pos, textures.get("gates").unwrap(), self.tex_info());
        if self.n_inputs > 3 {
            // let y_offset = (self.n_inputs as f32 - 1.) / 2. * 20.;
            let y_offset = (self.n_inputs as f32 / 2.).floor() * TILE_SIZE;
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
        let mut bboxes = vec![Rect::new(
            -TILE_SIZE,
            -TILE_SIZE,
            self.size().x + 2. * TILE_SIZE,
            self.size().y + 2. * TILE_SIZE,
        )];
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
                GateKind::Not => vec![vec2(0., TILE_SIZE)],
                GateKind::Or => vec![vec2(0., TILE_SIZE), vec2(0., 3. * TILE_SIZE)],
                GateKind::And => vec![vec2(0., TILE_SIZE), vec2(0., 3. * TILE_SIZE)],
            }
        } else if self.n_inputs == 3 {
            vec![
                vec2(0., 0.),
                vec2(0., 2. * TILE_SIZE),
                vec2(0., 4. * TILE_SIZE),
            ]
        } else {
            let mut input_positions = Vec::new();
            let skipping_center = self.n_inputs % 2 == 0;
            let n_tiles = if skipping_center {
                self.n_inputs
            } else {
                self.n_inputs - 1
            } as isize;
            let y_offset = (n_tiles - 4) / 2;
            for i in 0..=n_tiles {
                if skipping_center && i == n_tiles / 2 {
                    continue;
                }
                input_positions.push(vec2(0., (i - y_offset) as f32 * TILE_SIZE));
            }
            input_positions
        }
    }
    fn output_positions(&self) -> Vec<Vec2> {
        let tex_info = self.tex_info();
        vec![vec2(tex_info.size.x, tex_info.size.y / 2.)]
    }
    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        // let mut new_comp: CompUpdateResponse = None;
        let mut data_bits = self.data_bits;
        ComboBox::from_label("Data Bits")
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(self.kind, data_bits, self.n_inputs);
            return Some(None);
        }

        if !matches!(self.kind, GateKind::Not) {
            let mut n_inputs = self.n_inputs;
            ComboBox::from_label("Inputs")
                .width(COMBO_WIDTH)
                .selected_text(format!("{}", n_inputs))
                .show_ui(ui, |ui| {
                    for i in 1..=10 {
                        ui.selectable_value(&mut n_inputs, i, format!("{i}"));
                    }
                });
            if n_inputs != self.n_inputs {
                *self = Self::new(self.kind, self.data_bits, n_inputs);
                return Some(None);
            }
        }

        None
    }
}

impl Gate {
    fn new(kind: GateKind, data_bits: u8, n_inputs: usize) -> Self {
        Self {
            kind,
            data_bits,
            n_inputs,
            inputs: vec![Pin::new(data_bits); n_inputs],
            output: Pin::new(data_bits),
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
            GateKind::Not => {
                TexInfo::new(vec2(455., 117.), vec2(67., 65.), TILE_SIZE * vec2(3., 2.))
            }
            GateKind::And => TexInfo::new(vec2(75., 0.), vec2(82., 67.), TILE_SIZE * vec2(4., 4.)),
            GateKind::Or => TexInfo::new(vec2(72., 236.), vec2(82., 73.), TILE_SIZE * vec2(4., 4.)),
        }
    }
}

#[derive(Debug, Clone)]
struct Mux {
    sel_bits: u8,
    data_bits: u8,
    inputs: Vec<Pin>,
    output: Pin,
    selector: Pin,
}

impl Logic for Mux {
    fn name(&self) -> &'static str {
        "Multiplexer"
    }
    fn do_logic(&mut self) {
        let value = self.selector.get().and_then(|s| {
            let sel = s.load::<usize>();
            self.inputs[sel].get()
        });
        self.output.set(value);
    }

    fn n_in_pins(&self) -> usize {
        // Count the inputs and the selector pin
        self.inputs.len() + 1
    }

    fn n_out_pins(&self) -> usize {
        1
    }

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        match px {
            PinIndex::Input(i) => {
                // 0 -> selector, then inputs
                if i == 0 {
                    self.selector.get()
                } else {
                    self.inputs[i - 1].get()
                }
            }
            PinIndex::Output(0) => self.output.get(),
            _ => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        match px {
            PinIndex::Input(i) => {
                if i == 0 {
                    self.selector.set(value);
                } else {
                    self.inputs[i - 1].set(value)
                }
            }
            PinIndex::Output(0) => self.output.set(value),
            _ => panic!(),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        match px {
            PinIndex::Input(i) => {
                // 0 -> selector, then inputs
                if i == 0 {
                    self.selector.bits
                } else {
                    self.inputs[i - 1].bits
                }
            }
            PinIndex::Output(0) => self.output.bits,
            _ => panic!(),
        }
    }
}

impl Draw for Mux {
    fn size(&self) -> Vec2 {
        let width = if self.sel_bits == 1 { 3. } else { 4. };
        TILE_SIZE * Vec2::new(width, usize::max(self.inputs.len() + 2, 4) as f32)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();
        let ramp_y = if self.sel_bits == 1 {
            draw_line(
                pos.x + TILE_SIZE,
                pos.y + h,
                pos.x + TILE_SIZE,
                pos.y + h - TILE_SIZE / 3.,
                1.,
                BLACK,
            );
            TILE_SIZE
        } else {
            2. * TILE_SIZE
        };
        let a = pos;
        let b = pos + vec2(w, ramp_y);
        let c = pos + vec2(w, h - ramp_y);
        let d = pos + vec2(0., h);
        draw_line(a.x, a.y, b.x, b.y, 1., BLACK);
        draw_line(b.x, b.y, c.x, c.y, 1., BLACK);
        draw_line(c.x, c.y, d.x, d.y, 1., BLACK);
        draw_line(d.x, d.y, a.x, a.y, 1., BLACK);
    }

    fn input_positions(&self) -> Vec<Vec2> {
        if self.sel_bits == 1 {
            return vec![
                vec2(TILE_SIZE, self.size().y),
                vec2(0., TILE_SIZE),
                vec2(0., 3. * TILE_SIZE),
            ];
        }
        let mut input_positions = vec![vec2(2. * TILE_SIZE, self.size().y - TILE_SIZE)];
        let n_inputs = self.inputs.len();
        input_positions.extend((1..=n_inputs).map(|i| vec2(0., i as f32 * TILE_SIZE)));
        input_positions
    }
    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        let mut data_bits = self.data_bits;

        ComboBox::from_label("Data Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(self.sel_bits, data_bits);
            return Some(None);
        }

        let mut select_bits = self.sel_bits;
        ComboBox::from_label("Select Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{select_bits}"))
            .show_ui(ui, |ui| {
                for i in 1..=6 {
                    ui.selectable_value(&mut select_bits, i, format!("{i}"));
                }
            });
        if select_bits != self.sel_bits {
            *self = Self::new(select_bits, self.data_bits);
            return Some(None);
        }
        None
    }
}

impl Mux {
    fn new(sel_bits: u8, data_bits: u8) -> Self {
        Self {
            sel_bits,
            data_bits,
            inputs: vec![Pin::new(data_bits); 1 << sel_bits],
            output: Pin::new(data_bits),
            selector: Pin::new(sel_bits),
        }
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
    input: Pin,
    outputs: Vec<Pin>,
    selector: Pin,
}

impl Logic for Demux {
    fn name(&self) -> &'static str {
        "Demultiplexer"
    }
    fn do_logic(&mut self) {
        let sel = self.selector.get().map(|s| s.load::<usize>());

        if let Some(sel) = sel {
            for output in &mut self.outputs {
                if let Some(s) = &mut output.signal {
                    s.fill(false);
                }
            }

            self.outputs[sel].set(self.input.get())
        }
    }

    fn n_in_pins(&self) -> usize {
        2
    }

    fn n_out_pins(&self) -> usize {
        // Count selector as well
        self.outputs.len()
    }

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.selector.get(),
                1 => self.input.get(),
                _ => panic!(),
            },
            PinIndex::Output(i) => self.outputs[i].get(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.selector.set(value),
                1 => self.input.set(value),
                _ => panic!(),
            },
            PinIndex::Output(i) => self.outputs[i].set(value),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.selector.bits,
                1 => self.input.bits,
                _ => panic!(),
            },
            PinIndex::Output(i) => self.outputs[i].bits,
        }
    }
}

impl Draw for Demux {
    fn size(&self) -> Vec2 {
        let width = if self.sel_bits == 1 { 3. } else { 4. };
        TILE_SIZE * Vec2::new(width, usize::max(self.outputs.len() + 2, 4) as f32)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();
        let ramp_y = if self.sel_bits == 1 {
            draw_line(
                pos.x + 2. * TILE_SIZE,
                pos.y + h,
                pos.x + 2. * TILE_SIZE,
                pos.y + h - TILE_SIZE / 3.,
                1.,
                BLACK,
            );
            TILE_SIZE
        } else {
            2. * TILE_SIZE
        };
        let a = pos + vec2(0., ramp_y);
        let b = pos + vec2(w, 0.);
        let c = pos + vec2(w, h);
        let d = pos + vec2(0., h - ramp_y);
        draw_line(a.x, a.y, b.x, b.y, 1., BLACK);
        draw_line(b.x, b.y, c.x, c.y, 1., BLACK);
        draw_line(c.x, c.y, d.x, d.y, 1., BLACK);
        draw_line(d.x, d.y, a.x, a.y, 1., BLACK);
    }

    fn input_positions(&self) -> Vec<Vec2> {
        if self.sel_bits == 1 {
            vec![
                vec2(TILE_SIZE * 2., self.size().y),
                vec2(0., self.size().y / 2.),
            ]
        } else {
            vec![
                vec2(2. * TILE_SIZE, self.size().y - TILE_SIZE),
                vec2(0., self.size().y / 2.),
            ]
        }
    }

    fn output_positions(&self) -> Vec<Vec2> {
        if self.sel_bits == 1 {
            return vec![TILE_SIZE * vec2(3., 1.), TILE_SIZE * vec2(3., 3.)];
        }
        let n_outputs = self.outputs.len();
        (1..=n_outputs)
            .map(|i| TILE_SIZE * vec2(4., i as f32))
            .collect()
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        // let mut new_comp: CompUpdateResponse = None;
        // Group::new(hash!(), vec2(MENU_SIZE.x, 30.)).ui(ui, |ui| {
        //     // Data bits
        //     let mut data_bits_sel = self.data_bits as usize - 1;
        //     ui.combo_box(hash!(), "Data Bits", COMBO_OPTS, &mut data_bits_sel);
        //     let new_data_bits = data_bits_sel as u8 + 1;
        //
        //     if new_data_bits != self.data_bits {
        //         let demux = Self::new(self.sel_bits, new_data_bits);
        //         new_comp = Some(Box::new(demux));
        //     };
        // });

        let mut data_bits = self.data_bits;
        ComboBox::from_label("Data Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(self.sel_bits, data_bits);
            return Some(None);
        }

        let mut select_bits = self.sel_bits;
        ComboBox::from_label("Select Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{select_bits}"))
            .show_ui(ui, |ui| {
                for i in 1..=6 {
                    ui.selectable_value(&mut select_bits, i, format!("{i}"));
                }
            });
        if select_bits != self.sel_bits {
            *self = Self::new(select_bits, self.data_bits);
            return Some(None);
        }
        None
    }
}

impl Demux {
    fn new(sel_bits: u8, data_bits: u8) -> Self {
        Self {
            sel_bits,
            data_bits,
            input: Pin::new(data_bits),
            outputs: vec![Pin::new(data_bits); 1 << sel_bits],
            selector: Pin::new(sel_bits),
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
    write_enable: Pin,
    input: Pin,
    output: Pin,
}

impl Register {
    fn new(data_bits: u8) -> Self {
        Self {
            data_bits,
            write_enable: Pin::new(1),
            input: Pin::new(data_bits),
            output: Pin::new(data_bits),
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
        if let Some(s) = self.write_enable.get() {
            if s.any() {
                self.output.set(self.input.get());
            }
        }
    }

    fn n_in_pins(&self) -> usize {
        2
    }

    fn n_out_pins(&self) -> usize {
        1
    }

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.write_enable.get(),
                1 => self.input.get(),
                _ => panic!(),
            },
            PinIndex::Output(0) => self.output.get(),
            _ => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.write_enable.set(value),
                1 => self.input.set(value),
                _ => panic!(),
            },
            PinIndex::Output(0) => self.output.set(value),
            _ => panic!(),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        match px {
            PinIndex::Input(i) => match i {
                0 => self.write_enable.bits,
                1 => self.input.bits,
                _ => panic!(),
            },
            PinIndex::Output(0) => self.output.bits,
            _ => panic!(),
        }
    }
}

impl Draw for Register {
    fn size(&self) -> Vec2 {
        TILE_SIZE * Vec2::new(4., 6.)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();
        let in_color = self.input.color();
        draw_rectangle(pos.x, pos.y, w / 2., h / 2., in_color);
        let wen_color = self.write_enable.color();
        draw_rectangle(pos.x, pos.y + h / 2., w / 2., h / 2., wen_color);
        let out_color = self.output.color();
        draw_rectangle(pos.x + w / 2., pos.y, w / 2., h, out_color);
        draw_rectangle_lines(pos.x, pos.y, w, h, 2., BLACK);

        draw_text("D", pos.x + 2., pos.y + 25., 20., BLACK);
        draw_text("WE", pos.x + 2., pos.y + 45., 20., BLACK);
        draw_text("Q", pos.x + 28., pos.y + 25., 20., BLACK);
    }
    fn input_positions(&self) -> Vec<Vec2> {
        vec![
            Vec2::new(0., 4. * TILE_SIZE), // Write Enable
            Vec2::new(0., 2. * TILE_SIZE),
        ]
    }

    fn output_positions(&self) -> Vec<Vec2> {
        vec![Vec2::new(self.size().x, 2. * TILE_SIZE)]
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        let mut data_bits = self.data_bits;
        ComboBox::from_label("Data Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(data_bits);
            return Some(None);
        }
        None
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
    value: Pin,
}

impl Input {
    fn new(data_bits: u8) -> Self {
        Self {
            data_bits,
            value: Pin {
                bits: data_bits,
                signal: Some(signal_zeros(data_bits)),
            },
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

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        match px {
            PinIndex::Input(_) => panic!(),
            PinIndex::Output(0) => self.value.get(),
            _ => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        match px {
            PinIndex::Input(_) => panic!(),
            PinIndex::Output(0) => self.value.set(value),
            _ => panic!(),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        match px {
            PinIndex::Input(_) => panic!(),
            PinIndex::Output(0) => self.value.bits,
            _ => panic!(),
        }
    }

    fn interact(&mut self) -> bool {
        if let Some(s) = self.value.get() {
            let incremented = s.load::<u32>() + 1;
            let value = &incremented.view_bits::<Lsb0>()[..self.data_bits as usize];
            self.value.set(Some(value));
        }
        true
    }
}

impl Draw for Input {
    fn size(&self) -> Vec2 {
        TILE_SIZE * Vec2::new(2., 2.)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let color = self.value.color();
        draw_rectangle(pos.x, pos.y, self.size().x, self.size().y, color);
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        let mut data_bits = self.data_bits;
        ComboBox::from_label("Data Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(data_bits);
            return Some(None);
        }
        None
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
    value: Pin,
}

impl Output {
    fn new(data_bits: u8) -> Self {
        Self {
            data_bits,
            value: Pin::new(data_bits),
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

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        if let PinIndex::Input(0) = px {
            return self.value.get();
        } else {
            panic!()
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        if let PinIndex::Input(0) = px {
            self.value.set(value);
        } else {
            panic!()
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        if let PinIndex::Input(0) = px {
            self.value.bits
        } else {
            panic!()
        }
    }
}

impl Draw for Output {
    fn size(&self) -> Vec2 {
        TILE_SIZE * Vec2::new(2., 2.)
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let color = self.value.color();
        draw_rectangle(pos.x, pos.y, self.size().x, self.size().y, color);
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        let mut data_bits = self.data_bits;
        ComboBox::from_label("Data Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(data_bits);
            return Some(None);
        }
        None
    }
}

impl Default for Output {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Debug, Clone)]
struct Splitter {
    input: Pin,        // input.len = data_bits_in
    outputs: Vec<Pin>, // outputs[i].len = data_bits_out[i]
    data_bits_in: u8,
    data_bits_out: Vec<u8>,
    mapping: Vec<usize>, // mapping.len = data_bits_in; mapping[i] = idx of outputs to send input[i]
}

impl Splitter {
    fn new(data_bits_in: u8, data_bits_out: Vec<u8>, mapping: Vec<usize>) -> Self {
        assert!(mapping.len() == data_bits_in as usize);
        Self {
            input: Pin::new(data_bits_in),
            outputs: data_bits_out.iter().map(|&i| Pin::new(i)).collect(),
            data_bits_in,
            data_bits_out,
            mapping,
        }
    }
}

impl Default for Splitter {
    fn default() -> Self {
        Self::new(2, vec![1, 1], vec![0, 1])
    }
}

impl Logic for Splitter {
    fn name(&self) -> &'static str {
        "Splitter"
    }

    fn n_in_pins(&self) -> usize {
        1
    }

    fn n_out_pins(&self) -> usize {
        self.outputs.len()
    }

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        match px {
            PinIndex::Input(0) => self.input.get(),
            PinIndex::Output(i) => self.outputs[i].get(),
            _ => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        match px {
            PinIndex::Input(0) => self.input.set(value),
            PinIndex::Output(i) => self.outputs[i].set(value),
            _ => panic!(),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        match px {
            PinIndex::Input(0) => self.input.bits,
            PinIndex::Output(i) => self.outputs[i].bits,
            _ => panic!(),
        }
    }

    fn do_logic(&mut self) {
        match self.input.get() {
            Some(input) => {
                for output in &mut self.outputs {
                    match &mut output.signal {
                        Some(s) => s.clear(),
                        None => output.signal = Some(Signal::new()),
                    }
                }
                for (bit, &branch) in self.mapping.iter().enumerate() {
                    // Just instantiated all Some, so unwrap is okay
                    self.outputs[branch]
                        .signal
                        .as_mut()
                        .unwrap()
                        .push(input[bit]);
                }
            }
            None => {
                for output in &mut self.outputs {
                    output.signal = None
                }
            }
        }
    }
}

impl Draw for Splitter {
    fn size(&self) -> Vec2 {
        TILE_SIZE * Vec2::new(2., self.outputs.len() as f32)
    }

    fn input_positions(&self) -> Vec<Vec2> {
        vec![Vec2::new(0., self.size().y)]
    }

    fn output_positions(&self) -> Vec<Vec2> {
        (0..self.outputs.len())
            .map(|i| TILE_SIZE * vec2(2., i as f32))
            .collect()
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (w, h) = self.size().into();

        draw_line(
            pos.x,
            pos.y + h,
            pos.x + TILE_SIZE,
            pos.y + h - TILE_SIZE,
            3.,
            BLACK,
        );
        draw_line(
            pos.x + TILE_SIZE,
            pos.y,
            pos.x + TILE_SIZE,
            pos.y + h - TILE_SIZE,
            3.,
            BLACK,
        );
        for i in 0..self.outputs.len() {
            let i = i as f32;
            draw_line(
                pos.x + TILE_SIZE,
                pos.y + i * TILE_SIZE,
                pos.x + w,
                pos.y + i * TILE_SIZE,
                1.,
                BLACK,
            );
        }
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        let mut data_bits_in = self.data_bits_in;
        let n_outputs = self.outputs.len();
        ComboBox::from_label("Data Bits In")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits_in))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits_in, i, format!("{i}"));
                }
            });

        if data_bits_in != self.data_bits_in {
            let (new_data_bits_out, new_mapping) = if data_bits_in > self.data_bits_in {
                // add extra width to last arm, and map all the extra bits to it.
                let extra = data_bits_in - self.data_bits_in;
                let mut data_bits_out = self.data_bits_out.clone();
                data_bits_out[n_outputs - 1] += extra;
                let mut mapping = self.mapping.clone();
                mapping.extend(vec![n_outputs - 1; extra as usize]);
                (data_bits_out, mapping)
            } else {
                // truncate the mapping; recalculate data_bits_out
                let mapping = self.mapping[..data_bits_in as usize].to_vec();
                let mut data_bits_out = vec![0; self.outputs.len()];
                for &branch in &mapping {
                    data_bits_out[branch] += 1;
                }
                (data_bits_out, mapping)
            };
            *self = Self::new(data_bits_in, new_data_bits_out, new_mapping);
            return Some(None);
        }

        let mut new_n_outputs = self.outputs.len();
        ComboBox::from_label("Number of Arms")
            .width(COMBO_WIDTH)
            .selected_text(format!("{new_n_outputs}"))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut new_n_outputs, i, format!("{i}"));
                }
            });

        if new_n_outputs != n_outputs {
            let (new_data_bits_out, new_mapping) = if new_n_outputs > n_outputs {
                // make additional arms empty; don't change mapping
                let extra = new_n_outputs - n_outputs;
                let mut data_bits_out = self.data_bits_out.clone();
                data_bits_out.extend(vec![0; extra]);
                (data_bits_out, self.mapping.clone())
            } else {
                // truncate outputs; replace with last existing arm in mapping
                let mapping = self
                    .mapping
                    .iter()
                    .map(|&branch| {
                        if branch >= new_n_outputs {
                            new_n_outputs - 1
                        } else {
                            branch
                        }
                    })
                    .collect::<Vec<_>>();
                (self.data_bits_out[..new_n_outputs].to_vec(), mapping)
            };
            *self = Self::new(self.data_bits_in, new_data_bits_out, new_mapping);
            return Some(None);
        }
        ui.separator();
        for bit in 0..self.data_bits_in as usize {
            let arm = self.mapping[bit];
            let mut new_arm = arm;
            ComboBox::from_label(format!("Bit {}", bit))
                .width(COMBO_WIDTH)
                .width(50.)
                .selected_text(format!("{}", new_arm))
                .show_ui(ui, |ui| {
                    for i in 0..n_outputs {
                        ui.selectable_value(&mut new_arm, i, format!("{i}"));
                    }
                });

            if new_arm != arm {
                self.mapping[bit] = new_arm;
                return Some(None);
            }
        }
        None
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TunnelKind {
    #[default]
    Sender,
    Receiver,
}

impl Display for TunnelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                TunnelKind::Sender => "Sender",
                TunnelKind::Receiver => "Receiver",
            }
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Tunnel {
    kind: TunnelKind,
    label: String,
    live_label: String,
    data_bits: u8,
    value: Pin,
}

impl Tunnel {
    fn new(kind: TunnelKind, label: String, data_bits: u8) -> Self {
        Self {
            kind,
            live_label: label.clone(),
            label,
            data_bits,
            value: Pin::new(data_bits),
        }
    }
}

impl Default for Tunnel {
    fn default() -> Self {
        Self::new(TunnelKind::default(), String::new(), 1)
    }
}

impl Logic for Tunnel {
    fn name(&self) -> &'static str {
        "Tunnel"
    }

    fn n_in_pins(&self) -> usize {
        match self.kind {
            TunnelKind::Sender => 1,
            TunnelKind::Receiver => 0,
        }
    }

    fn n_out_pins(&self) -> usize {
        match self.kind {
            TunnelKind::Sender => 0,
            TunnelKind::Receiver => 1,
        }
    }

    fn get_pin_value(&self, px: PinIndex) -> Option<SignalRef> {
        // Tunnels have one pin that is both input and output
        match px {
            PinIndex::Input(0) | PinIndex::Output(0) => self.value.get(),
            _ => panic!(),
        }
    }

    fn set_pin_value(&mut self, px: PinIndex, value: Option<SignalRef>) {
        // Tunnels have one pin that is both input and output
        match px {
            PinIndex::Input(0) | PinIndex::Output(0) => self.value.set(value),
            _ => panic!(),
        }
    }

    fn get_pin_width(&self, px: PinIndex) -> u8 {
        // Tunnels have one pin that is both input and output
        match px {
            PinIndex::Input(0) | PinIndex::Output(0) => self.value.bits,
            _ => panic!(),
        }
    }

    fn do_logic(&mut self) {}

    fn get_ctx_event(&mut self, event: CompEvent) -> Option<CtxEvent> {
        match event {
            CompEvent::Added => Some(CtxEvent::TunnelUpdate(TunnelUpdate {
                label: self.label.clone(),
                tunnel_kind: self.kind,
                update_kind: TunnelUpdateKind::Add,
            })),
            CompEvent::Removed => Some(CtxEvent::TunnelUpdate(TunnelUpdate {
                label: self.label.clone(),
                tunnel_kind: self.kind,
                update_kind: TunnelUpdateKind::Remove,
            })),
        }
    }
}

impl Draw for Tunnel {
    fn size(&self) -> Vec2 {
        let text_dims = measure_text(&self.label, None, 15, 1.);
        Vec2::new(
            f32::max(4. * TILE_SIZE, 2. * TILE_SIZE + text_dims.width.ceil()),
            2. * TILE_SIZE,
        )
    }

    fn draw(&self, pos: Vec2, _: &HashMap<&str, Texture2D>) {
        let (x, y) = pos.into();
        let (w, h) = self.size().into();

        draw_text(
            &self.label,
            pos.x + TILE_SIZE,
            pos.y + TILE_SIZE * 1.5,
            15.,
            BLACK,
        );
        // Draw arrow shape pointing either left or right dependinging on TunnelKind
        let points = match self.kind {
            TunnelKind::Sender => [
                (x, y + TILE_SIZE),
                (x + TILE_SIZE, y),
                (x + w, y),
                (x + w, y + h),
                (x + TILE_SIZE, y + h),
            ],
            TunnelKind::Receiver => [
                (x, y),
                (x + w - TILE_SIZE, y),
                (x + w, y + TILE_SIZE),
                (x + w - TILE_SIZE, y + h),
                (x, y + h),
            ],
        };
        for i in 0..points.len() - 1 {
            draw_line(
                points[i].0,
                points[i].1,
                points[i + 1].0,
                points[i + 1].1,
                1.,
                BLACK,
            );
        }
        draw_line(
            points[4].0,
            points[4].1,
            points[0].0,
            points[0].1,
            1.,
            BLACK,
        );
    }

    fn draw_properties_ui(&mut self, ui: &mut Ui) -> CompUpdateResponse {
        let mut data_bits = self.data_bits;
        ComboBox::from_label("Data Bits")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", data_bits))
            .show_ui(ui, |ui| {
                for i in 1..=32 {
                    ui.selectable_value(&mut data_bits, i, format!("{i}"));
                }
            });

        if data_bits != self.data_bits {
            *self = Self::new(self.kind, self.label.clone(), data_bits);
            return Some(None);
        }

        let response = ui.text_edit_singleline(&mut self.live_label);
        if response.lost_focus() {
            let rename_tunnel_event = CtxEvent::TunnelUpdate(TunnelUpdate {
                label: self.label.clone(),
                tunnel_kind: self.kind,
                update_kind: TunnelUpdateKind::Rename(self.live_label.clone()),
            });
            let result = Some(Some(rename_tunnel_event));
            self.label = self.live_label.clone();
            return result;
        }

        let mut kind = self.kind;

        ComboBox::from_label("Kind")
            .width(COMBO_WIDTH)
            .selected_text(format!("{}", kind))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut kind, TunnelKind::Sender, "Sender");
                ui.selectable_value(&mut kind, TunnelKind::Receiver, "Receiver");
            });

        if kind != self.kind {
            let flip_tunnel_event = CtxEvent::TunnelUpdate(TunnelUpdate {
                label: self.label.clone(),
                tunnel_kind: self.kind,
                update_kind: TunnelUpdateKind::Flip,
            });
            self.kind = kind;
            return Some(Some(flip_tunnel_event));
        }

        None
    }
}

pub(crate) fn color_from_signal(sig: Option<SignalRef>) -> Color {
    match sig {
        Some(s) => {
            if s.any() {
                DARKGREEN
            } else {
                BLUE
            }
        }
        None => RED,
    }
}

pub fn default_comp_from_name(comp_name: &str) -> Component {
    let kind: Box<dyn Comp> = match comp_name {
        "NOT" => Box::new(Gate::default_of_kind(GateKind::Not)),
        "AND" => Box::new(Gate::default_of_kind(GateKind::And)),
        "OR" => Box::new(Gate::default_of_kind(GateKind::Or)),
        "Input" => Box::new(Input::default()),
        "Output" => Box::new(Output::default()),
        "Register" => Box::new(Register::default()),
        "Mux" => Box::new(Mux::default()),
        "Demux" => Box::new(Demux::default()),
        "Splitter" => Box::new(Splitter::default()),
        "Tunnel" => Box::new(Tunnel::default()),
        _ => {
            panic!("Unknown component attempted to be created.")
        }
    };

    Component::new(kind, Vec2::ZERO)
}

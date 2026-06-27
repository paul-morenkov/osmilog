use crate::net::{Net, NetKey};
use crate::value::Value::{self, Floating};
use slotmap::{new_key_type, SlotMap};

new_key_type! {
    pub struct CompKey;
}

#[derive(Debug)]
pub struct Component {
    pub pins: Pins,
    pub logic: Logic,
}

impl Component {
    // TODO: Consider returning pin index information
    pub fn input(value: Value) -> Self {
        Self {
            pins: Pins::new(0, 1),
            logic: Logic::Input(value),
        }
    }
    pub fn output() -> Self {
        Self {
            pins: Pins::new(1, 0),
            logic: Logic::Output,
        }
    }

    pub fn gate(op: GateOp, n: usize, width: u8) -> Self {
        Self {
            pins: Pins::new(n, 1),
            logic: Logic::Gate { op, width },
        }
    }

    pub fn mux(data_width: u8, sel_width: u8) -> Self {
        let branches = 1 << sel_width;
        Self {
            pins: Pins::new(branches + 1, 1),
            logic: Logic::Mux {
                data_width,
                sel_width,
            },
        }
    }

    pub fn demux(data_width: u8, sel_width: u8) -> Self {
        Self {
            pins: Pins::new(2, 1 << sel_width),
            logic: Logic::Demux {
                data_width,
                sel_width,
            },
        }
    }

    pub fn evaluate(&self, nets: &SlotMap<NetKey, Net>) -> Vec<Value> {
        let read_pin = |i: usize| -> Value {
            match self.pins.inputs[i] {
                Some(net) => nets[net].value,
                None => Value::Floating,
            }
        };

        let n_inputs = self.pins.inputs.len();

        match &self.logic {
            Logic::Input(val) => vec![*val],
            Logic::Output => vec![],
            Logic::Gate { op, width } => {
                let val = match op {
                    GateOp::And | GateOp::Nand => {
                        let mut acc = Value::Fixed {
                            bits: Value::mask(*width),
                            width: *width,
                        };
                        for i in 0..n_inputs {
                            let x = read_pin(i);
                            acc = acc & x;
                        }
                        if matches!(op, GateOp::Nand) {
                            !acc
                        } else {
                            acc
                        }
                    }
                    GateOp::Or | GateOp::Nor => {
                        let mut acc = Value::Fixed {
                            bits: 0,
                            width: *width,
                        };
                        for i in 0..n_inputs {
                            acc = acc | read_pin(i)
                        }
                        if matches!(op, GateOp::Nor) {
                            !acc
                        } else {
                            acc
                        }
                    }
                    GateOp::Xor | GateOp::Xnor => {
                        let mut acc = Value::Fixed {
                            bits: 0,
                            width: *width,
                        };
                        for i in 0..n_inputs {
                            acc = acc ^ read_pin(i)
                        }
                        if matches!(op, GateOp::Xnor) {
                            !acc
                        } else {
                            acc
                        }
                    }
                    GateOp::Not => !read_pin(0),
                };
                vec![val] // Assumes single output
            }
            Logic::Mux { sel_width, .. } => match read_pin(0) {
                Value::Floating => vec![Value::Floating],
                Value::Fixed { bits, width } => {
                    if *sel_width == width {
                        vec![read_pin(bits as usize + 1)]
                    } else {
                        vec![Value::Floating]
                    }
                }
            },
            // Demux: inputs[0] => data, inputs[1] => selector
            Logic::Demux {
                sel_width,
                data_width,
            } => {
                let branches = 1 << sel_width;
                match read_pin(1) {
                    Value::Fixed { bits: sel, width } if width == *sel_width => {
                        let mut values = vec![Value::new(0, *data_width); branches];
                        // TODO: check data_width?
                        values[sel as usize] = read_pin(0);
                        values
                    }
                    _ => vec![Value::Floating; branches],
                }
            }
            Logic::Reg => todo!(),
        }
    }

    pub fn net_of(&self, pin: PinId) -> Option<NetKey> {
        match pin {
            // TODO: will panic on out of bounds, fix this
            PinId::In(i) => self.pins.inputs[i.0 as usize],
            PinId::Out(i) => self.pins.outputs[i.0 as usize],
        }
    }
    pub fn set_pin_net(&mut self, pin: PinId, net: NetKey) {
        match pin {
            PinId::In(i) => self.pins.inputs[i.0 as usize] = Some(net),
            PinId::Out(i) => self.pins.outputs[i.0 as usize] = Some(net),
        };
    }

    pub fn is_sequential(&self) -> bool {
        match self.logic {
            Logic::Gate { .. }
            | Logic::Mux { .. }
            | Logic::Demux { .. }
            | Logic::Input(_)
            | Logic::Output => false,
            Logic::Reg => true,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct InIdx(pub u8);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct OutIdx(pub u8);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PinId {
    In(InIdx),
    Out(OutIdx),
}

impl PinId {
    pub fn input(i: u8) -> Self {
        Self::In(InIdx(i))
    }
    pub fn output(i: u8) -> Self {
        Self::Out(OutIdx(i))
    }
}

#[derive(Debug)]
pub struct Pins {
    pub inputs: Vec<Option<NetKey>>,
    pub outputs: Vec<Option<NetKey>>,
    pub out_cache: Vec<Value>, // TODO: Should this be combined with outputs to enforce the same
                               // lengths?
}

impl Pins {
    pub fn new(inputs: usize, outputs: usize) -> Self {
        Self {
            inputs: vec![None; inputs],
            outputs: vec![None; outputs],
            out_cache: vec![Value::default(); outputs],
        }
    }
}

#[derive(Debug)]
pub enum Logic {
    Input(Value),
    Output,
    Gate { op: GateOp, width: u8 },
    Mux { data_width: u8, sel_width: u8 },
    Demux { data_width: u8, sel_width: u8 },
    Reg,
}

#[derive(Debug)]
pub enum GateOp {
    And,
    Or,
    Xor,
    Xnor,
    Nand,
    Nor,
    Not,
}

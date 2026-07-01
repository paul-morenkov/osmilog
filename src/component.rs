use crate::net::{Net, NetKey};
use crate::value::Value;
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
    pub fn input(bits: u32, width: u8) -> Self {
        Self {
            pins: Pins::new(0, 1),
            logic: Logic::Comb(LogicComb::Input { bits, width }),
        }
    }
    pub fn output() -> Self {
        Self {
            pins: Pins::new(1, 0),
            logic: Logic::Comb(LogicComb::Output),
        }
    }

    pub fn gate(op: GateOp, n: usize, width: u8) -> Self {
        Self {
            pins: Pins::new(n, 1),
            logic: Logic::Comb(LogicComb::Gate { op, width }),
        }
    }

    pub fn mux(data_width: u8, sel_width: u8) -> Self {
        let branches = 1 << sel_width;
        Self {
            pins: Pins::new(branches + 1, 1),
            logic: Logic::Comb(LogicComb::Mux {
                data_width,
                sel_width,
            }),
        }
    }

    pub fn demux(data_width: u8, sel_width: u8) -> Self {
        Self {
            pins: Pins::new(2, 1 << sel_width),
            logic: Logic::Comb(LogicComb::Demux {
                data_width,
                sel_width,
            }),
        }
    }

    pub fn reg(data_width: u8) -> Self {
        Self {
            pins: Pins::new(2, 1), // [0] data, [1] write_enable -> [0] value
            logic: Logic::Seq(LogicSeq::Reg {
                value: Value::new(0, data_width),
                data_width,
            }),
        }
    }

    // Reads the current Value of every input pin from net state, without mutating
    // anything. Used by evaluate() and by Circuit::tick_clock()'s input-collection stage.
    pub fn read_inputs(&self, nets: &SlotMap<NetKey, Net>) -> Vec<Value> {
        self.pins
            .inputs
            .iter()
            .map(|slot| match slot {
                Some(net) => nets[*net].value,
                None => Value::Floating,
            })
            .collect()
    }

    pub fn evaluate(&self, nets: &SlotMap<NetKey, Net>) -> Vec<Value> {
        let inputs = self.read_inputs(nets);
        let read_pin = |i: usize| -> Value { inputs[i] };

        let n_inputs = inputs.len();

        match &self.logic {
            Logic::Comb(comb) => match comb {
                LogicComb::Input { bits, width } => vec![Value::Fixed {
                    bits: *bits,
                    width: *width,
                }],
                LogicComb::Output => vec![],
                LogicComb::Gate { op, width } => {
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
                LogicComb::Mux { sel_width, .. } => match read_pin(0) {
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
                LogicComb::Demux {
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
            },
            // Sequential components never mutate state or recompute outputs via the
            // combinational path (add_component / attach / neighboring net changes) -
            // they just report their currently latched value(s). State only changes
            // via tick().
            Logic::Seq(seq) => match seq {
                LogicSeq::Reg { value, .. } => vec![*value],
            },
        }
    }

    // Advances one clock tick given pre-collected input values (see read_inputs).
    // Mutates persisted state and returns new out_cache values. Only valid on
    // Logic::Seq components - callers must filter with is_sequential() first.
    pub fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        match &mut self.logic {
            Logic::Comb(_) => unreachable!("tick() called on a combinational component"),
            Logic::Seq(seq) => match seq {
                LogicSeq::Reg { value, .. } => {
                    let data = inputs[0];
                    let write_enable = inputs[1];
                    if matches!(write_enable, Value::Fixed { bits: 1, width: 1 }) {
                        *value = data;
                    }
                    vec![*value]
                }
            },
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
        matches!(self.logic, Logic::Seq(_))
    }

    pub(crate) fn clear_pins(&mut self) {
        for input in &mut self.pins.inputs {
            *input = None;
        }
        for output in &mut self.pins.outputs {
            *output = None;
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
    Comb(LogicComb),
    Seq(LogicSeq),
}

#[derive(Debug)]
pub enum LogicComb {
    Input { bits: u32, width: u8 }, // Matches Value::Fixed
    Output,
    Gate { op: GateOp, width: u8 },
    Mux { data_width: u8, sel_width: u8 },
    Demux { data_width: u8, sel_width: u8 },
}

#[derive(Debug)]
pub enum LogicSeq {
    Reg { value: Value, data_width: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOp {
    And,
    Or,
    Xor,
    Xnor,
    Nand,
    Nor,
    Not,
}

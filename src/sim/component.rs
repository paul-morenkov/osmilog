use crate::sim::net::{Net, NetKey};
use crate::sim::value::Value;
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

    // arm_bits[j] lists the data-bit indices routed to arm j, e.g.
    // arm_bits = [[0, 2], [1, 3]] sends bits 0,2 to arm0 and bits 1,3 to arm1.
    // Arm indices are just positions in `arm_bits`, so an out-of-range arm
    // reference isn't representable. If a bit index is listed in more than one
    // arm, the later arm (by position in `arm_bits`) wins.
    pub fn splitter(arm_bits: Vec<Vec<u8>>) -> Self {
        let arms = arm_bits.len() as u8;
        let width = arm_bits
            .iter()
            .flatten()
            .map(|&bit| bit + 1)
            .max()
            .unwrap_or(0);
        let mut in_out_map = vec![None; width as usize];
        for (arm, bits) in arm_bits.into_iter().enumerate() {
            for bit in bits {
                in_out_map[bit as usize] = Some(arm as u8);
            }
        }
        Self {
            pins: Pins::new(1, arms as usize),
            logic: Logic::Comb(LogicComb::Splitter(Splitter { arms, in_out_map })),
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
                LogicComb::Splitter(s) => match read_pin(0) {
                    // TODO: Use `width` to validate total width in arms
                    Value::Fixed { bits, .. } => {
                        let mut out = Vec::new();
                        for out_arm in 0..s.arms {
                            let mut out_bits = 0;
                            let mut out_width = 0;
                            for (data_i, &arm) in s.in_out_map.iter().enumerate() {
                                if arm == Some(out_arm) {
                                    let is_set = bits & (1 << data_i) > 0;
                                    if is_set {
                                        out_bits |= 1 << out_width;
                                    }
                                    out_width += 1;
                                }
                            }
                            out.push(Value::new(out_bits, out_width));
                        }
                        out
                    }
                    Value::Floating => {
                        vec![Value::Floating; s.arms as usize]
                    }
                },
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
    Splitter(Splitter),
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

#[derive(Debug)]
pub struct Splitter {
    arms: u8,
    // in_out_map[i] = Some(j) => the i'th bit of data goes to arm j; None => unrouted.
    // Only constructible via Component::splitter(), which builds this from an
    // arm-major Vec<Vec<u8>> so every arm index here is guaranteed valid.
    in_out_map: Vec<Option<u8>>,
}

impl Splitter {
    pub fn data_width(&self) -> u8 {
        self.in_out_map.len() as u8
    }
}

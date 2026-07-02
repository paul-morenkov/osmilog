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

    pub fn splitter(data_width: u8, arms: u8, in_out_map: Vec<u8>) -> Self {
        // FIXME: Decide how to pass `in_out_map` in to maximize correctness.
        Self {
            pins: Pins::new(1, arms as usize),
            logic: Logic::Comb(LogicComb::Splitter(Splitter {
                data_width,
                arms,
                in_out_map,
            })),
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
                    Value::Fixed { bits, width } => {
                        let mut out = Vec::new();
                        for out_arm in 0..s.arms {
                            let mut out_bits = 0;
                            let mut out_width = 0;
                            for (data_i, &arm) in s.in_out_map.iter().enumerate() {
                                if arm == out_arm {
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
    pub data_width: u8,
    pub arms: u8,
    pub in_out_map: Vec<u8>, // in_out_map[i] = j => The i'th bit of data goes to arm j
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // Directly drives Component::evaluate() with a single input net, bypassing
    // Circuit/settle(). Splitter is purely combinational and only reads input[0],
    // so this is enough to exercise the logic in isolation.
    fn splitter_outputs(input: Value, arms: u8, in_out_map: Vec<u8>) -> Vec<Value> {
        let mut nets: SlotMap<NetKey, Net> = SlotMap::with_key();
        let net = nets.insert(Net {
            value: input,
            ..Default::default()
        });
        let data_width = in_out_map.len() as u8;
        let mut splitter = Component::splitter(data_width, arms, in_out_map);
        splitter.set_pin_net(PinId::input(0), net);
        splitter.evaluate(&nets)
    }

    // ---- Group 1: contiguous split (low half / high half of a 4-bit bus) ----

    #[test_case(0b0000, vec![0, 0] ; "0000")]
    #[test_case(0b0001, vec![1, 0] ; "0001 -> low bit0")]
    #[test_case(0b0010, vec![2, 0] ; "0010 -> low bit1")]
    #[test_case(0b0011, vec![3, 0] ; "0011 -> low half full")]
    #[test_case(0b0100, vec![0, 1] ; "0100 -> high bit0")]
    #[test_case(0b1000, vec![0, 2] ; "1000 -> high bit1")]
    #[test_case(0b1100, vec![0, 3] ; "1100 -> high half full")]
    #[test_case(0b1111, vec![3, 3] ; "1111 -> both halves full")]
    fn test_splitter_contiguous_halves(data: u32, expected: Vec<u32>) {
        // 4-bit bus split into two 2-bit arms: bits [0,1] -> arm0, bits [2,3] -> arm1.
        let out = splitter_outputs(Value::new(data, 4), 2, vec![0, 0, 1, 1]);
        assert_eq!(
            out,
            vec![Value::new(expected[0], 2), Value::new(expected[1], 2)]
        );
    }

    // ---- Group 2: interleaved split (even bits -> arm0, odd bits -> arm1) ----

    #[test_case(0b0000, vec![0, 0] ; "0000")]
    #[test_case(0b0001, vec![1, 0] ; "bit0 (even) -> arm0 pos0")]
    #[test_case(0b0100, vec![2, 0] ; "bit2 (even) -> arm0 pos1")]
    #[test_case(0b0010, vec![0, 1] ; "bit1 (odd) -> arm1 pos0")]
    #[test_case(0b1000, vec![0, 2] ; "bit3 (odd) -> arm1 pos1")]
    #[test_case(0b1010, vec![0, 3] ; "bits1,3 -> arm1 full")]
    #[test_case(0b1111, vec![3, 3] ; "all bits set")]
    fn test_splitter_interleaved(data: u32, expected: Vec<u32>) {
        // 4-bit bus, even bits (0,2) -> arm0, odd bits (1,3) -> arm1.
        let out = splitter_outputs(Value::new(data, 4), 2, vec![0, 1, 0, 1]);
        assert_eq!(
            out,
            vec![Value::new(expected[0], 2), Value::new(expected[1], 2)]
        );
    }

    // ---- Group 3: full spread (every bit routed to its own single-bit arm) ----

    #[test_case(0b0000, vec![0, 0, 0, 0] ; "all zero")]
    #[test_case(0b0001, vec![1, 0, 0, 0] ; "bit0 set")]
    #[test_case(0b0010, vec![0, 1, 0, 0] ; "bit1 set")]
    #[test_case(0b0100, vec![0, 0, 1, 0] ; "bit2 set")]
    #[test_case(0b1000, vec![0, 0, 0, 1] ; "bit3 set")]
    #[test_case(0b1111, vec![1, 1, 1, 1] ; "all bits set")]
    fn test_splitter_full_spread(data: u32, expected: Vec<u32>) {
        // Each of the 4 bits fans out to its own dedicated 1-bit arm.
        let out = splitter_outputs(Value::new(data, 4), 4, vec![0, 1, 2, 3]);
        assert_eq!(
            out,
            expected
                .into_iter()
                .map(|b| Value::new(b, 1))
                .collect::<Vec<_>>()
        );
    }

    // ---- Group 4: edge cases ----

    #[test]
    fn test_splitter_floating_input_propagates_to_all_arms() {
        let out = splitter_outputs(Value::Floating, 3, vec![0, 1, 2]);
        assert_eq!(out, vec![Value::Floating; 3]);
    }

    #[test]
    fn test_splitter_zero_arms_produces_empty_output() {
        let out = splitter_outputs(Value::new(0, 2), 0, vec![]);
        assert_eq!(out, Vec::<Value>::new());
    }

    #[test]
    fn test_splitter_arm_with_no_mapped_bits_is_zero_width() {
        // arm 2 never appears in in_out_map, so it should receive nothing.
        let out = splitter_outputs(Value::new(0b11, 2), 3, vec![0, 1]);
        assert_eq!(out[2], Value::new(0, 0));
    }

    #[test]
    fn test_splitter_in_out_map_shorter_than_data_ignores_extra_bits() {
        // in_out_map only covers the low 2 bits of a nominally-4-bit value;
        // the upper bits (2,3) are unmapped and should have no effect on any arm.
        let out = splitter_outputs(Value::new(0b1101, 4), 2, vec![0, 1]);
        assert_eq!(out, vec![Value::new(1, 1), Value::new(0, 1)]);
    }

    #[test]
    fn test_splitter_out_of_range_arm_index_is_dropped_silently() {
        // in_out_map routes bit1 to arm index 5, but the component only has 2 arms
        // (0..2), so that bit is never picked up by any out_arm and is dropped
        // rather than panicking on an out-of-bounds output.
        let out = splitter_outputs(Value::new(0b11, 2), 2, vec![0, 5]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], Value::new(1, 1));
    }
}

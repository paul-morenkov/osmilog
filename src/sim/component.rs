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
    fn from_comb(logic: LogicComb) -> Self {
        let pins = Pins::new(logic.n_inputs(), logic.n_outputs());
        Self {
            pins,
            logic: Logic::Comb(logic),
        }
    }

    fn from_seq(logic: LogicSeq) -> Self {
        let pins = Pins::new(logic.n_inputs(), logic.n_outputs());
        Self {
            pins,
            logic: Logic::Seq(logic),
        }
    }

    pub fn input(bits: u32, width: u8) -> Self {
        Self::from_comb(LogicComb::Input(Input { bits, width }))
    }
    pub fn output() -> Self {
        Self::from_comb(LogicComb::Output)
    }

    pub fn gate(op: GateOp, n: usize, width: u8) -> Self {
        Self::from_comb(LogicComb::Gate(Gate {
            op,
            n_inputs: n,
            width,
        }))
    }

    pub fn mux(data_width: u8, sel_width: u8) -> Self {
        Self::from_comb(LogicComb::Mux(Mux {
            data_width,
            sel_width,
        }))
    }

    pub fn demux(data_width: u8, sel_width: u8) -> Self {
        Self::from_comb(LogicComb::Demux(Demux {
            data_width,
            sel_width,
        }))
    }

    pub fn reg(data_width: u8) -> Self {
        Self::from_seq(LogicSeq::Reg {
            config: Reg { data_width },
            value: Value::new(0, data_width),
        })
    }

    // arm_bits[j] lists the data-bit indices routed to arm j, e.g.
    // arm_bits = [[0, 2], [1, 3]] sends bits 0,2 to arm0 and bits 1,3 to arm1.
    // Arm indices are just positions in `arm_bits`, so an out-of-range arm
    // reference isn't representable. If a bit index is listed in more than one
    // arm, the later arm (by position in `arm_bits`) wins. `direction` picks
    // which side of this bit-to-arm mapping is the input: Right keeps the
    // classic splitter shape (1 input bus -> arms outputs); Left inverts it
    // into a combiner (arms inputs -> 1 output bus), using the same mapping.
    pub fn splitter(arm_bits: Vec<Vec<u8>>, direction: FanDirection) -> Self {
        let arms = arm_bits.len() as u8;
        let width = arm_bits
            .iter()
            .flatten()
            .map(|&bit| bit + 1)
            .max()
            .unwrap_or(0);
        let mut owner = vec![None; width as usize];
        for (arm, bits) in arm_bits.into_iter().enumerate() {
            for bit in bits {
                owner[bit as usize] = Some(arm as u8);
            }
        }
        // routing[i] = Some((arm, slot)) => data bit i is arm `arm`'s `slot`-th
        // bit (0 = LSB of that arm's Value), in ascending data-bit order per
        // arm. Precomputed once here (rather than recounted on every
        // evaluate() call) since it depends only on arm_bits' structure.
        let mut arm_width = vec![0u8; arms as usize];
        let routing: Vec<Option<(u8, u8)>> = owner
            .into_iter()
            .map(|maybe_arm| {
                maybe_arm.map(|arm| {
                    let slot = arm_width[arm as usize];
                    arm_width[arm as usize] += 1;
                    (arm, slot)
                })
            })
            .collect();

        Self::from_comb(LogicComb::Splitter(Splitter {
            arms,
            direction,
            routing,
            arm_width,
        }))
    }

    pub fn priority_encoder(sel_width: u8) -> Self {
        Self::from_comb(LogicComb::Encoder(Encoder { sel_width }))
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
        match &self.logic {
            Logic::Comb(comb) => comb.evaluate(&inputs),
            // Sequential components never mutate state or recompute outputs via the
            // combinational path (add_component / attach / neighboring net changes) -
            // they just report their currently latched value(s). State only changes
            // via tick().
            Logic::Seq(seq) => seq.observe(),
        }
    }

    // Advances one clock tick given pre-collected input values (see read_inputs).
    // Mutates persisted state and returns new out_cache values. Only valid on
    // Logic::Seq components - callers must filter with is_sequential() first.
    pub fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        match &mut self.logic {
            Logic::Comb(_) => unreachable!("tick() called on a combinational component"),
            Logic::Seq(seq) => seq.tick(inputs),
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

// Each LogicComb variant wraps a struct (named after the component itself) that owns its
// construction parameters, its own pin arity (n_inputs/n_outputs), and its own evaluate() -
// together in one place, so a component's declared pin count and what evaluate() actually
// reads/returns can't drift apart the way a separate constructor and evaluate() match arm
// could. Pins::new() in Component::from_comb() is sized directly from these same methods.
#[derive(Debug)]
pub enum LogicComb {
    Input(Input),
    Output,
    Gate(Gate),
    Mux(Mux),
    Demux(Demux),
    Splitter(Splitter),
    Encoder(Encoder),
}

impl LogicComb {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Input(_) => 0,
            Self::Output => 1,
            Self::Gate(g) => g.n_inputs(),
            Self::Mux(m) => m.n_inputs(),
            Self::Demux(d) => d.n_inputs(),
            Self::Splitter(s) => s.n_inputs(),
            Self::Encoder(e) => e.n_inputs(),
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Input(_) => 1,
            Self::Output => 0,
            Self::Gate(g) => g.n_outputs(),
            Self::Mux(m) => m.n_outputs(),
            Self::Demux(d) => d.n_outputs(),
            Self::Splitter(s) => s.n_outputs(),
            Self::Encoder(e) => e.n_outputs(),
        }
    }

    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match self {
            Self::Input(p) => vec![Value::Fixed {
                bits: p.bits,
                width: p.width,
            }],
            Self::Output => vec![],
            Self::Gate(g) => g.evaluate(inputs),
            Self::Mux(m) => m.evaluate(inputs),
            Self::Demux(d) => d.evaluate(inputs),
            Self::Splitter(s) => s.evaluate(inputs),
            Self::Encoder(e) => e.evaluate(inputs),
        }
    }
}

// LogicSeq mirrors LogicComb, except its config struct (Reg) holds only static construction
// parameters - the mutable latched `value` lives alongside it in the enum variant, not inside
// the struct, so ComponentDef (a visual *construction* record) can embed the config struct
// directly without also carrying simulated runtime state around (see gui/placed_component.rs).
#[derive(Debug)]
pub enum LogicSeq {
    Reg { config: Reg, value: Value },
}

impl LogicSeq {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Reg { config, .. } => config.n_inputs(),
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Reg { config, .. } => config.n_outputs(),
        }
    }

    pub fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        match self {
            Self::Reg { value, .. } => {
                let data = inputs[0];
                let write_enable = inputs[1];
                if matches!(write_enable, Value::Fixed { bits: 1, width: 1 }) {
                    *value = data;
                }
                vec![*value]
            }
        }
    }

    pub fn observe(&self) -> Vec<Value> {
        match self {
            Self::Reg { value, .. } => vec![*value],
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    pub bits: u32,
    pub width: u8,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Gate {
    pub op: GateOp,
    pub n_inputs: usize,
    pub width: u8,
}

impl Gate {
    pub fn n_inputs(&self) -> usize {
        self.n_inputs
    }
    pub fn n_outputs(&self) -> usize {
        1
    }
    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let width = self.width;
        let val = match self.op {
            GateOp::And | GateOp::Nand => {
                let mut acc = Value::Fixed {
                    bits: Value::mask(width),
                    width,
                };
                for &x in inputs {
                    acc = acc & x;
                }
                if matches!(self.op, GateOp::Nand) {
                    !acc
                } else {
                    acc
                }
            }
            GateOp::Or | GateOp::Nor => {
                let mut acc = Value::Fixed { bits: 0, width };
                for &x in inputs {
                    acc = acc | x;
                }
                if matches!(self.op, GateOp::Nor) {
                    !acc
                } else {
                    acc
                }
            }
            GateOp::Xor | GateOp::Xnor => {
                let mut acc = Value::Fixed { bits: 0, width };
                for &x in inputs {
                    acc = acc ^ x;
                }
                if matches!(self.op, GateOp::Xnor) {
                    !acc
                } else {
                    acc
                }
            }
            GateOp::Not => !inputs[0],
        };
        vec![val] // Assumes single output
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mux {
    pub data_width: u8,
    pub sel_width: u8,
}

impl Mux {
    pub fn n_inputs(&self) -> usize {
        (1usize << self.sel_width) + 1
    }
    pub fn n_outputs(&self) -> usize {
        1
    }
    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match inputs[0] {
            Value::Floating => vec![Value::Floating],
            Value::Fixed { bits, width } => {
                if self.sel_width == width {
                    vec![inputs[bits as usize + 1]]
                } else {
                    vec![Value::Floating]
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Demux {
    pub data_width: u8,
    pub sel_width: u8,
}

impl Demux {
    pub fn n_inputs(&self) -> usize {
        2
    }
    pub fn n_outputs(&self) -> usize {
        1usize << self.sel_width
    }
    // inputs[0] => data, inputs[1] => selector
    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let branches = 1 << self.sel_width;
        match inputs[1] {
            Value::Fixed { bits: sel, width } if width == self.sel_width => {
                let mut values = vec![Value::new(0, self.data_width); branches];
                // TODO: check data_width?
                values[sel as usize] = inputs[0];
                values
            }
            _ => vec![Value::Floating; branches],
        }
    }
}

// Inputs: 0 -> enable_in; 1.. -> arms (2^sel_width of them)
// Outputs: 0 -> selector; 1 -> enable_out; 2 -> group_out
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Encoder {
    pub sel_width: u8,
}

impl Encoder {
    pub fn n_inputs(&self) -> usize {
        (1usize << self.sel_width) + 1
    }
    pub fn n_outputs(&self) -> usize {
        3
    }
    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        let enable_in = inputs[0];
        let n_arms = 1 << self.sel_width;

        match enable_in {
            // Malformed enable_in (wrong bit width): the whole component goes Floating.
            Value::Fixed { width, .. } if width != 1 => {
                vec![Value::Floating, Value::Floating, Value::Floating]
            }
            // Explicitly disabled: selector = Floating, EN_OUT/GRP_OUT = 0.
            Value::Fixed { bits: 0, width: 1 } => {
                vec![Value::Floating, Value::new(0, 1), Value::new(0, 1)]
            }
            // Enabled: Floating or Fixed{width:1, bits != 0}.
            _ => {
                let highest_set = (1..n_arms + 1)
                    .map(|i| inputs[i])
                    .rposition(|v| v == Value::new(1, 1));

                if let Some(i) = highest_set {
                    // If an input is 1: selector = i, EN_OUT = 0, GRP_OUT = 1.
                    vec![
                        Value::new(i as u32, self.sel_width),
                        Value::new(0, 1),
                        Value::new(1, 1),
                    ]
                } else {
                    // If enabled but no inputs 1: selector = Floating, EN_OUT = 1, GRP_OUT = 0.
                    vec![Value::Floating, Value::new(1, 1), Value::new(0, 1)]
                }
            }
        }
    }
}

// Register config only - the latched runtime value lives in LogicSeq::Reg::value, not here,
// so this struct stays a pure construction record (embeddable directly in ComponentDef).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reg {
    pub data_width: u8,
}

impl Reg {
    pub fn n_inputs(&self) -> usize {
        2
    }
    pub fn n_outputs(&self) -> usize {
        1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GateOp {
    And,
    Or,
    Xor,
    Xnor,
    Nand,
    Nor,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FanDirection {
    Right,
    Left,
}

#[derive(Debug)]
pub struct Splitter {
    arms: u8,
    direction: FanDirection,
    // routing[i] = Some((arm, slot)) => the i'th data bit is arm `arm`'s
    // `slot`-th bit; None => unrouted (dropped in Right mode, always 0 in
    // the merged Left-mode trunk). Only constructible via
    // Component::splitter(), which builds this from an arm-major
    // Vec<Vec<u8>> so every arm index here is guaranteed valid.
    routing: Vec<Option<(u8, u8)>>,
    // arm_width[j] = number of data bits owned by arm j. In Right mode this
    // is the width evaluate() gives that arm's output; in Left mode it's
    // the width evaluate() requires on that arm's input.
    arm_width: Vec<u8>,
}

impl Splitter {
    pub fn data_width(&self) -> u8 {
        self.routing.len() as u8
    }
    pub fn direction(&self) -> FanDirection {
        self.direction
    }
    pub fn n_inputs(&self) -> usize {
        match self.direction {
            FanDirection::Right => 1,
            FanDirection::Left => self.arms as usize,
        }
    }
    pub fn n_outputs(&self) -> usize {
        match self.direction {
            FanDirection::Right => self.arms as usize,
            FanDirection::Left => 1,
        }
    }
    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match self.direction {
            FanDirection::Right => match inputs[0] {
                Value::Fixed { bits, .. } => {
                    let mut arm_bits_out = vec![0u32; self.arms as usize];
                    for (data_i, route) in self.routing.iter().enumerate() {
                        if let Some((arm, slot)) = *route {
                            if bits & (1 << data_i) != 0 {
                                arm_bits_out[arm as usize] |= 1 << slot;
                            }
                        }
                    }
                    (0..self.arms as usize)
                        .map(|arm| Value::new(arm_bits_out[arm], self.arm_width[arm]))
                        .collect()
                }
                Value::Floating => vec![Value::Floating; self.arms as usize],
            },
            FanDirection::Left => {
                let arm_vals: Vec<Value> = (0..self.arms as usize).map(|i| inputs[i]).collect();
                // Every arm that owns >=1 bit must be driven at exactly the
                // width it owns; Floating or a width mismatch poisons the
                // whole merged output, mirroring Value's own bitwise-op
                // width-mismatch convention rather than silently
                // truncating/zero-extending.
                let widths_ok = (0..self.arms as usize).all(|arm| {
                    self.arm_width[arm] == 0
                        || matches!(arm_vals[arm], Value::Fixed { width, .. } if width == self.arm_width[arm])
                });
                if !widths_ok {
                    vec![Value::Floating]
                } else {
                    let mut out_bits = 0u32;
                    for (data_i, route) in self.routing.iter().enumerate() {
                        if let Some((arm, slot)) = *route {
                            if let Value::Fixed { bits, .. } = arm_vals[arm as usize] {
                                if bits & (1 << slot) != 0 {
                                    out_bits |= 1 << data_i;
                                }
                            }
                        }
                    }
                    vec![Value::new(out_bits, self.data_width())]
                }
            }
        }
    }
}

use crate::sim::net::{Net, NetKey};
use crate::sim::value::Value;
use slotmap::{new_key_type, SlotMap};

mod adder;
mod demux;
mod encoder;
mod gate;
mod input;
mod mux;
mod reg;
mod splitter;
mod subtractor;

pub use adder::Adder;
pub use demux::Demux;
pub use encoder::Encoder;
pub use gate::{Gate, GateOp};
pub use input::Input;
pub use mux::Mux;
pub use reg::Reg;
pub use splitter::{FanDirection, Splitter};
pub use subtractor::Subtractor;

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

    pub fn splitter(arm_bits: Vec<Vec<u8>>, direction: FanDirection) -> Self {
        Self::from_comb(LogicComb::Splitter(Splitter::new(arm_bits, direction)))
    }

    pub fn priority_encoder(sel_width: u8) -> Self {
        Self::from_comb(LogicComb::Encoder(Encoder { sel_width }))
    }

    pub fn adder(data_width: u8) -> Self {
        Self::from_comb(LogicComb::Adder(Adder { data_width }))
    }

    pub fn subtractor(data_width: u8) -> Self {
        Self::from_comb(LogicComb::Subtractor(Subtractor { data_width }))
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

    pub fn input_width(&self, i: InIdx) -> Option<u8> {
        match &self.logic {
            Logic::Comb(c) => c.input_width(i.0 as usize),
            Logic::Seq(s) => s.input_width(i.0 as usize),
        }
    }

    pub fn output_width(&self, i: OutIdx) -> Option<u8> {
        match &self.logic {
            Logic::Comb(c) => c.output_width(i.0 as usize),
            Logic::Seq(s) => s.output_width(i.0 as usize),
        }
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

// Each LogicComb variant (other than the parameterless Output) wraps a struct, named after the
// component itself, that implements CombLogic: its construction parameters, its own pin arity
// (n_inputs/n_outputs), and its own evaluate() live together in one place, so a component's
// declared pin count and what evaluate() actually reads/returns can't drift apart the way a
// separate constructor and evaluate() match arm could. The trait (rather than separate inherent
// impls) makes that trio a compiler-checked contract: forgetting evaluate() on a new component
// struct is a "trait not implemented" error, not a silent gap only caught when some match arm
// tries to call it. Pins::new() in Component::from_comb() is sized directly from these methods.
pub trait CombLogic {
    fn n_inputs(&self) -> usize;
    fn n_outputs(&self) -> usize;
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value>;
    // Expected bit width of input/output pin `i`, from this component's own construction
    // parameters (not from any Value currently on a net). None means the pin accepts/produces
    // any width (currently only Output). Used by Circuit::resolve_net() to flag a Net whose
    // attached pins disagree on width, independent of whether a concrete Value is present.
    fn input_width(&self, i: usize) -> Option<u8>;
    fn output_width(&self, i: usize) -> Option<u8>;
}

#[derive(Debug)]
pub enum LogicComb {
    Input(Input),
    Output,
    Gate(Gate),
    Mux(Mux),
    Demux(Demux),
    Splitter(Splitter),
    Encoder(Encoder),
    Adder(Adder),
    Subtractor(Subtractor),
}

impl LogicComb {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Input(p) => p.n_inputs(),
            Self::Output => 1,
            Self::Gate(g) => g.n_inputs(),
            Self::Mux(m) => m.n_inputs(),
            Self::Demux(d) => d.n_inputs(),
            Self::Splitter(s) => s.n_inputs(),
            Self::Encoder(e) => e.n_inputs(),
            Self::Adder(a) => a.n_inputs(),
            Self::Subtractor(s) => s.n_inputs(),
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Input(p) => p.n_outputs(),
            Self::Output => 0,
            Self::Gate(g) => g.n_outputs(),
            Self::Mux(m) => m.n_outputs(),
            Self::Demux(d) => d.n_outputs(),
            Self::Splitter(s) => s.n_outputs(),
            Self::Encoder(e) => e.n_outputs(),
            Self::Adder(a) => a.n_outputs(),
            Self::Subtractor(s) => s.n_outputs(),
        }
    }

    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match self {
            Self::Input(p) => p.evaluate(inputs),
            Self::Output => vec![],
            Self::Gate(g) => g.evaluate(inputs),
            Self::Mux(m) => m.evaluate(inputs),
            Self::Demux(d) => d.evaluate(inputs),
            Self::Splitter(s) => s.evaluate(inputs),
            Self::Encoder(e) => e.evaluate(inputs),
            Self::Adder(a) => a.evaluate(inputs),
            Self::Subtractor(s) => s.evaluate(inputs),
        }
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Input(p) => p.input_width(i),
            Self::Output => None,
            Self::Gate(g) => g.input_width(i),
            Self::Mux(m) => m.input_width(i),
            Self::Demux(d) => d.input_width(i),
            Self::Splitter(s) => s.input_width(i),
            Self::Encoder(e) => e.input_width(i),
            Self::Adder(a) => a.input_width(i),
            Self::Subtractor(s) => s.input_width(i),
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Input(p) => p.output_width(i),
            Self::Output => None,
            Self::Gate(g) => g.output_width(i),
            Self::Mux(m) => m.output_width(i),
            Self::Demux(d) => d.output_width(i),
            Self::Splitter(s) => s.output_width(i),
            Self::Encoder(e) => e.output_width(i),
            Self::Adder(a) => a.output_width(i),
            Self::Subtractor(s) => s.output_width(i),
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

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Reg { config, .. } => match i {
                0 => Some(config.data_width), // data
                _ => Some(1),                 // write_enable
            },
        }
    }

    pub fn output_width(&self, _i: usize) -> Option<u8> {
        match self {
            Self::Reg { config, .. } => Some(config.data_width),
        }
    }
}

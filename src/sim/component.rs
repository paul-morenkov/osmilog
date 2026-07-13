use crate::sim::circuit::Circuit;
use crate::sim::net::{Net, NetKey};
use crate::sim::value::Value;
use slotmap::{new_key_type, SlotMap};

mod adder;
mod comparator;
mod constant;
mod counter;
mod d_flip_flop;
mod demux;
mod divider;
mod encoder;
mod gate;
mod input;
mod jk_flip_flop;
mod multiplier;
mod mux;
mod ram;
mod reg;
mod rom;
mod shift_reg;
mod splitter;
mod sr_flip_flop;
mod subtractor;
mod t_flip_flop;

pub use adder::Adder;
pub use comparator::Comparator;
pub use constant::Constant;
pub use counter::{Counter, CounterConf, OverflowAction};
pub use d_flip_flop::{DFlipFlop, DFlipFlopConf};
pub use demux::Demux;
pub use divider::Divider;
pub use encoder::Encoder;
pub use gate::{Gate, GateOp};
pub use input::Input;
pub use jk_flip_flop::{JKFlipFlop, JKFlipFlopConf};
pub use multiplier::Multiplier;
pub use mux::Mux;
pub use ram::{Ram, RamCell, ReadBehavior};
pub use reg::{Reg, RegConf};
pub use rom::{Rom, MAX_ADDRESS_WIDTH};
pub use shift_reg::{ShiftReg, ShiftRegConf};
pub use splitter::{FanDirection, Splitter};
pub use sr_flip_flop::{SRFlipFlop, SRFlipFlopConf};
pub use subtractor::Subtractor;
pub use t_flip_flop::{TFlipFlop, TFlipFlopConf};

new_key_type! {
    pub struct CompKey;
}

new_key_type! {
    /// Opaque reference to another circuit document, embedded in
    /// `ComponentSpec::Subcircuit`. The simulator never dereferences it: the GUI
    /// owns the document registry (`SlotMap<DocId, CircuitDoc>`) and builds a
    /// subcircuit's inner `Circuit` from the referenced document. Defined here
    /// (not in the GUI) so `ComponentSpec` can carry it without `sim` depending
    /// on `gui`; re-exported from `gui::document`.
    pub struct DocId;
}

#[derive(Debug)]
pub struct Component {
    pub pins: Pins,
    pub logic: Logic,
    // `false` marks a tombstone: kept so CompKey stays valid and a Reg's
    // latched state survives, but skipped by whole-component sweeps
    // (tick_clock, clear_nets). See Circuit::remove_component/reactivate_component.
    pub(crate) active: bool,
}

impl Component {
    fn from_comb(logic: LogicComb) -> Self {
        let pins = Pins::new(logic.n_inputs(), logic.n_outputs());
        Self {
            pins,
            logic: Logic::Comb(logic),
            active: true,
        }
    }

    fn from_seq(logic: LogicSeq) -> Self {
        let pins = Pins::new(logic.n_inputs(), logic.n_outputs());
        Self {
            pins,
            logic: Logic::Seq(logic),
            active: true,
        }
    }

    pub fn input(bits: u32, width: u8) -> Self {
        Self::from_comb(LogicComb::Input(Input { bits, width }))
    }
    pub fn constant(bits: u32, width: u8) -> Self {
        Self::from_comb(LogicComb::Constant(Constant { bits, width }))
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
        Self::from_seq(LogicSeq::Reg(Reg::new(data_width)))
    }

    pub fn shift_reg(data_width: u8, num_stages: usize, parallel_load: bool) -> Self {
        Self::from_seq(LogicSeq::ShiftReg(ShiftReg::new(
            data_width,
            num_stages,
            parallel_load,
        )))
    }

    pub fn d_flip_flop() -> Self {
        Self::from_seq(LogicSeq::DFlipFlop(DFlipFlop::new()))
    }

    pub fn t_flip_flop() -> Self {
        Self::from_seq(LogicSeq::TFlipFlop(TFlipFlop::new()))
    }

    pub fn jk_flip_flop() -> Self {
        Self::from_seq(LogicSeq::JKFlipFlop(JKFlipFlop::new()))
    }

    pub fn sr_flip_flop() -> Self {
        Self::from_seq(LogicSeq::SRFlipFlop(SRFlipFlop::new()))
    }

    pub fn counter(data_width: u8, max_value: u32, overflow_action: OverflowAction) -> Self {
        Self::from_seq(LogicSeq::Counter(Counter::new(
            data_width,
            max_value,
            overflow_action,
        )))
    }

    pub fn splitter(arm_bits: Vec<Vec<u8>>, direction: FanDirection) -> Self {
        Self::from_comb(LogicComb::Splitter(Splitter::new(arm_bits, direction)))
    }

    // Builds a subcircuit component wrapping a whole inner Circuit. `inputs` /
    // `outputs` are the inner Input / Output component keys in the pin order
    // this component exposes (the GUI derives that top-down); pin arity comes
    // straight from their lengths. See Logic::Sub / SubCircuit.
    pub fn subcircuit(inner: Circuit, inputs: Vec<CompKey>, outputs: Vec<CompKey>) -> Self {
        let pins = Pins::new(inputs.len(), outputs.len());
        Self {
            pins,
            logic: Logic::Sub(SubCircuit {
                inner,
                inputs,
                outputs,
            }),
            active: true,
        }
    }

    // A subcircuit component with the given pin arity but an empty inner
    // circuit (no boundary keys), so it settles to all-Floating outputs. Only a
    // safe fallback for ComponentSpec::to_component() on a Subcircuit spec; the
    // GUI builds real subcircuits via gui::app::instantiate.
    pub fn subcircuit_placeholder(n_inputs: usize, n_outputs: usize) -> Self {
        Self {
            pins: Pins::new(n_inputs, n_outputs),
            logic: Logic::Sub(SubCircuit {
                inner: Circuit::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
            }),
            active: true,
        }
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

    pub fn multiplier(data_width: u8) -> Self {
        Self::from_comb(LogicComb::Multiplier(Multiplier { data_width }))
    }

    pub fn divider(data_width: u8) -> Self {
        Self::from_comb(LogicComb::Divider(Divider { data_width }))
    }

    pub fn comparator(data_width: u8) -> Self {
        Self::from_comb(LogicComb::Comparator(Comparator { data_width }))
    }

    // Builds a ROM from a full Rom record (widths + contents). Takes the record
    // by value so to_component() can hand over a clone of the placed spec's data.
    pub fn rom(rom: Rom) -> Self {
        Self::from_comb(LogicComb::Rom(rom))
    }

    // Builds a RAM from a full Ram record (widths + read_behavior + shared
    // contents handle). Takes the record by value, same as rom() - see Ram's
    // docs for why the buffer aliases the placed spec instead of copying it.
    pub fn ram(ram: Ram) -> Self {
        Self::from_seq(LogicSeq::Ram(RamCell::new(ram)))
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
            // Sequential components report their latched value here; any async
            // input effect (e.g. a reset) was already folded into that state by
            // apply_async(), and clocked changes only happen in tick().
            Logic::Seq(seq) => seq.observe(),
            // A subcircuit's inner Circuit was already driven+settled by
            // apply_async() (settle-time) or tick() (clock-time), so this is a
            // pure read of its boundary Output components.
            Logic::Sub(sub) => sub.observe(),
        }
    }

    // Applies a sequential component's asynchronous, level-sensitive logic
    // (e.g. an async reset) to its latched state, given current inputs. No-op
    // for combinational components. Called by Circuit::settle() so an async
    // input can take effect without a clock tick; must be idempotent (see
    // SeqLogic::apply_async).
    pub fn apply_async(&mut self, inputs: &[Value]) {
        match &mut self.logic {
            Logic::Seq(seq) => seq.apply_async(inputs),
            // Drive the boundary Input components with the current inputs and
            // settle the inner circuit. Idempotent: driving the same values
            // marks nothing dirty and settle() becomes a no-op.
            Logic::Sub(sub) => sub.drive_and_settle(inputs),
            Logic::Comb(_) => {}
        }
    }

    // Latched output values of a sequential component, reported without
    // recomputing from inputs (what evaluate() dispatches to for Logic::Seq).
    // Only valid on Logic::Seq components - callers must filter with
    // is_sequential() first.
    pub fn observe(&self) -> Vec<Value> {
        match &self.logic {
            Logic::Comb(_) => unreachable!("observe() called on a combinational component"),
            Logic::Seq(seq) => seq.observe(),
            Logic::Sub(sub) => sub.observe(),
        }
    }

    // Advances one clock tick given pre-collected input values (see read_inputs).
    // Mutates persisted state and returns new out_cache values. Only valid on
    // Logic::Seq components - callers must filter with is_sequential() first.
    pub fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        match &mut self.logic {
            Logic::Comb(_) => unreachable!("tick() called on a combinational component"),
            Logic::Seq(seq) => seq.tick(inputs),
            // Forward the clock edge into the inner circuit, then read its
            // boundary Output components as the new latched values.
            Logic::Sub(sub) => sub.tick(inputs),
        }
    }

    // Restores latched sequential state to its power-on initial value. Only
    // valid on Logic::Seq components - callers must filter with is_sequential().
    pub fn reset(&mut self) {
        match &mut self.logic {
            Logic::Comb(_) => unreachable!("reset() called on a combinational component"),
            Logic::Seq(seq) => seq.reset(),
            Logic::Sub(sub) => sub.reset(),
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

    pub fn is_subcircuit(&self) -> bool {
        matches!(self.logic, Logic::Sub(_))
    }

    // Components whose state advances on a clock tick and whose async effects
    // run inside settle(): sequential components and subcircuits. The engine's
    // whole-component sweeps (eval_component's apply_async, tick_clock,
    // reset_sequential) key on this rather than is_sequential().
    pub fn is_stateful(&self) -> bool {
        matches!(self.logic, Logic::Seq(_) | Logic::Sub(_))
    }

    pub fn input_width(&self, i: InIdx) -> Option<u8> {
        match &self.logic {
            Logic::Comb(c) => c.input_width(i.0 as usize),
            Logic::Seq(s) => s.input_width(i.0 as usize),
            Logic::Sub(sub) => sub.input_width(i.0 as usize),
        }
    }

    pub fn output_width(&self, i: OutIdx) -> Option<u8> {
        match &self.logic {
            Logic::Comb(c) => c.output_width(i.0 as usize),
            Logic::Seq(s) => s.output_width(i.0 as usize),
            Logic::Sub(sub) => sub.output_width(i.0 as usize),
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

// The canonical "construction params" record for a component - one variant
// per type, holding just enough to rebuild an equivalent `Component` via
// `to_component()` (inverse: `Component::spec`). Reused unmodified as the
// GUI's placed-component record: gui::placed_component adds a second
// inherent impl with GUI-only display methods, no wrapper/newtype needed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ComponentSpec {
    Input(Input),
    Constant(Constant),
    Output,
    Gate(Gate),
    Mux(Mux),
    Demux(Demux),
    Reg(RegConf),
    ShiftReg(ShiftRegConf),
    Encoder(Encoder),
    Adder(Adder),
    Subtractor(Subtractor),
    Multiplier(Multiplier),
    Divider(Divider),
    Comparator(Comparator),
    Rom(Rom),
    Ram(Ram),
    DFlipFlop(DFlipFlopConf),
    TFlipFlop(TFlipFlopConf),
    JKFlipFlop(JKFlipFlopConf),
    SRFlipFlop(SRFlipFlopConf),
    Counter(CounterConf),
    Splitter {
        // The trunk width being edited in the GUI properties panel,
        // independent of how many bits `arm_bits` actually assigns.
        // to_component() never reads this back - Splitter::new derives the
        // real data_width from arm_bits alone.
        width: u8,
        arm_bits: Vec<Vec<u8>>,
        direction: FanDirection,
    },
    // A whole other circuit document, placed as a component. `doc` is the link;
    // `name` and the per-pin widths are a *cached derived interface* (like a
    // Rom caching its contents in the spec) so all the `&self` spec methods
    // (n_inputs/size/shape/...) work without a document registry. The cache is
    // refreshed from the referenced document on switch-back
    // (gui::app::refresh_subcircuits). The live inner Circuit is NOT here - it's
    // built GUI-side (gui::app::build_circuit_from_doc) into Logic::Sub. `doc`
    // is #[serde(skip)] because a DocId is an ephemeral slotmap key with no
    // cross-reload meaning: on save the cross-circuit link is emitted as a plain
    // index (io::SubcircuitRef, into ProjectFile::circuits) and re-bound to a
    // freshly-allocated DocId on load (gui::app::load_project_file).
    Subcircuit {
        #[serde(skip)]
        doc: DocId,
        name: String,
        input_widths: Vec<u8>,
        output_widths: Vec<u8>,
    },
}

impl ComponentSpec {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Input(_) => 0,
            Self::Constant(_) => 0,
            Self::Output => 1,
            Self::Gate(g) => g.n_inputs(),
            Self::Mux(m) => m.n_inputs(),
            Self::Demux(d) => d.n_inputs(),
            Self::Reg(r) => r.n_inputs(),
            Self::ShiftReg(sr) => sr.n_inputs(),
            Self::Encoder(e) => e.n_inputs(),
            Self::Adder(a) => a.n_inputs(),
            Self::Subtractor(s) => s.n_inputs(),
            Self::Multiplier(m) => m.n_inputs(),
            Self::Divider(d) => d.n_inputs(),
            Self::Comparator(c) => c.n_inputs(),
            Self::Rom(r) => r.n_inputs(),
            Self::Ram(r) => r.n_inputs(),
            Self::DFlipFlop(ff) => ff.n_inputs(),
            Self::TFlipFlop(ff) => ff.n_inputs(),
            Self::JKFlipFlop(ff) => ff.n_inputs(),
            Self::SRFlipFlop(ff) => ff.n_inputs(),
            Self::Counter(c) => c.n_inputs(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => match direction {
                FanDirection::Right => 1,
                FanDirection::Left => arm_bits.len(),
            },
            Self::Subcircuit { input_widths, .. } => input_widths.len(),
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Input(_) => 1,
            Self::Constant(_) => 1,
            Self::Output => 0,
            Self::Gate(g) => g.n_outputs(),
            Self::Mux(m) => m.n_outputs(),
            Self::Demux(d) => d.n_outputs(),
            Self::Reg(r) => r.n_outputs(),
            Self::ShiftReg(sr) => sr.n_outputs(),
            Self::Encoder(e) => e.n_outputs(),
            Self::Adder(a) => a.n_outputs(),
            Self::Subtractor(s) => s.n_outputs(),
            Self::Multiplier(m) => m.n_outputs(),
            Self::Divider(d) => d.n_outputs(),
            Self::Comparator(c) => c.n_outputs(),
            Self::Rom(r) => r.n_outputs(),
            Self::Ram(r) => r.n_outputs(),
            Self::DFlipFlop(ff) => ff.n_outputs(),
            Self::TFlipFlop(ff) => ff.n_outputs(),
            Self::JKFlipFlop(ff) => ff.n_outputs(),
            Self::SRFlipFlop(ff) => ff.n_outputs(),
            Self::Counter(c) => c.n_outputs(),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => match direction {
                FanDirection::Right => arm_bits.len(),
                FanDirection::Left => 1,
            },
            Self::Subcircuit { output_widths, .. } => output_widths.len(),
        }
    }

    pub(crate) fn to_component(&self) -> Component {
        match self {
            Self::Input(p) => Component::input(p.bits, p.width),
            Self::Constant(c) => Component::constant(c.bits, c.width),
            Self::Output => Component::output(),
            Self::Gate(g) => Component::gate(g.op, g.n_inputs, g.width),
            Self::Mux(m) => Component::mux(m.data_width, m.sel_width),
            Self::Demux(d) => Component::demux(d.data_width, d.sel_width),
            Self::Reg(r) => Component::reg(r.data_width),
            Self::ShiftReg(sr) => {
                Component::shift_reg(sr.data_width, sr.num_stages, sr.parallel_load)
            }
            Self::Encoder(e) => Component::priority_encoder(e.sel_width),
            Self::Adder(a) => Component::adder(a.data_width),
            Self::Subtractor(s) => Component::subtractor(s.data_width),
            Self::Multiplier(m) => Component::multiplier(m.data_width),
            Self::Divider(d) => Component::divider(d.data_width),
            Self::Comparator(c) => Component::comparator(c.data_width),
            // shared(), not clone(): the live component and the placed spec
            // deliberately share one buffer (see Rom's docs). Every other
            // spec->component build owns its params outright, but a ROM's bulk
            // contents are too big to duplicate.
            Self::Rom(r) => Component::rom(r.shared()),
            // Same aliasing as Rom above (see Ram's docs).
            Self::Ram(r) => Component::ram(r.shared()),
            Self::DFlipFlop(_) => Component::d_flip_flop(),
            Self::TFlipFlop(_) => Component::t_flip_flop(),
            Self::JKFlipFlop(_) => Component::jk_flip_flop(),
            Self::SRFlipFlop(_) => Component::sr_flip_flop(),
            Self::Counter(c) => Component::counter(c.data_width, c.max_value, c.overflow_action),
            Self::Splitter {
                arm_bits,
                direction,
                ..
            } => Component::splitter(arm_bits.clone(), *direction),
            // A real subcircuit needs its inner Circuit built from the
            // referenced document, which requires the GUI's document registry;
            // that build goes through gui::app::instantiate, not here. This
            // arm is only reached if to_component() is called on a Subcircuit
            // spec directly - it yields a correctly-pinned but empty (all
            // outputs Floating) placeholder rather than panicking.
            Self::Subcircuit {
                input_widths,
                output_widths,
                ..
            } => Component::subcircuit_placeholder(input_widths.len(), output_widths.len()),
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
    pub out_cache: Vec<Value>,
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
    // A whole Circuit simulated as one component - a third kind alongside
    // combinational and sequential, because it both propagates combinationally
    // (its inner circuit needs &mut to settle) and holds clocked state (it
    // forwards clock ticks inward). See SubCircuit.
    Sub(SubCircuit),
}

// The runtime state of a subcircuit component: the owned inner Circuit plus the
// boundary Input / Output component keys, in this component's pin order (the
// GUI derives that order top-down from the inner Input/Output positions). Built
// GUI-side from a referenced document; never cloned or serialized, because
// Circuit is neither - the authoritative, persistable form lives at the
// ComponentSpec::Subcircuit / Wiring layer, and the inner Circuit is rebuilt
// from it (see gui::app::build_circuit_from_doc).
#[derive(Debug)]
pub struct SubCircuit {
    pub inner: Circuit,
    pub inputs: Vec<CompKey>,
    pub outputs: Vec<CompKey>,
}

impl SubCircuit {
    // Feed each boundary Input with the enclosing circuit's input values and
    // settle. drive_input marks a net dirty only when the value actually
    // changes, so re-running this with identical inputs settles nothing - the
    // idempotence apply_async() requires. A settle error is swallowed here; it
    // surfaces on the enclosing circuit's own settle() via the same nets.
    fn drive_and_settle(&mut self, inputs: &[Value]) {
        self.drive_inputs(inputs);
        let _ = self.inner.settle();
    }

    fn drive_inputs(&mut self, inputs: &[Value]) {
        for (i, &key) in self.inputs.iter().enumerate() {
            let value = inputs.get(i).copied().unwrap_or(Value::Floating);
            self.inner.drive_input(key, value);
        }
    }

    // The current values on the boundary Output components. Pure read (&self):
    // the inner circuit is already settled by the time evaluate()/observe()
    // call this.
    fn observe(&self) -> Vec<Value> {
        self.outputs
            .iter()
            .map(|&key| self.inner.read_output(key))
            .collect()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        self.drive_inputs(inputs);
        let _ = self.inner.tick_clock();
        self.observe()
    }

    fn reset(&mut self) {
        let _ = self.inner.reset_sequential();
    }

    // Boundary input width = the inner Input component's own output width.
    fn input_width(&self, i: usize) -> Option<u8> {
        self.inputs
            .get(i)
            .and_then(|&key| self.inner.components.get(key))
            .and_then(|c| c.output_width(OutIdx(0)))
    }

    // Boundary output widths are left unconstrained (like a bare Output): the
    // driven Value carries its own width, and the inner Output declares no
    // fixed width to check against.
    fn output_width(&self, _i: usize) -> Option<u8> {
        None
    }
}

// Each LogicComb variant (besides Output) wraps a struct implementing
// CombLogic: construction params, pin arity, and evaluate() live together so
// they can't drift apart. The trait makes that a compiler-checked contract -
// forgetting evaluate() on a new type is a "trait not implemented" error,
// not a silent gap. Pins::new() sizes directly from these methods.
pub trait CombLogic {
    fn n_inputs(&self) -> usize;
    fn n_outputs(&self) -> usize;
    fn evaluate(&self, inputs: &[Value]) -> Vec<Value>;
    // Expected bit width of pin `i`, from construction params (not any live
    // Value). None means any width (currently only Output). Used by
    // Circuit::resolve_net() to flag width-disagreeing nets.
    fn input_width(&self, i: usize) -> Option<u8>;
    fn output_width(&self, i: usize) -> Option<u8>;
}

#[derive(Debug)]
pub enum LogicComb {
    Input(Input),
    Constant(Constant),
    Output,
    Gate(Gate),
    Mux(Mux),
    Demux(Demux),
    Splitter(Splitter),
    Encoder(Encoder),
    Adder(Adder),
    Subtractor(Subtractor),
    Multiplier(Multiplier),
    Divider(Divider),
    Comparator(Comparator),
    Rom(Rom),
}

impl LogicComb {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Input(p) => p.n_inputs(),
            Self::Constant(c) => c.n_inputs(),
            Self::Output => 1,
            Self::Gate(g) => g.n_inputs(),
            Self::Mux(m) => m.n_inputs(),
            Self::Demux(d) => d.n_inputs(),
            Self::Splitter(s) => s.n_inputs(),
            Self::Encoder(e) => e.n_inputs(),
            Self::Adder(a) => a.n_inputs(),
            Self::Subtractor(s) => s.n_inputs(),
            Self::Multiplier(m) => m.n_inputs(),
            Self::Divider(d) => d.n_inputs(),
            Self::Comparator(c) => c.n_inputs(),
            Self::Rom(r) => r.n_inputs(),
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Input(p) => p.n_outputs(),
            Self::Constant(c) => c.n_outputs(),
            Self::Output => 0,
            Self::Gate(g) => g.n_outputs(),
            Self::Mux(m) => m.n_outputs(),
            Self::Demux(d) => d.n_outputs(),
            Self::Splitter(s) => s.n_outputs(),
            Self::Encoder(e) => e.n_outputs(),
            Self::Adder(a) => a.n_outputs(),
            Self::Subtractor(s) => s.n_outputs(),
            Self::Multiplier(m) => m.n_outputs(),
            Self::Divider(d) => d.n_outputs(),
            Self::Comparator(c) => c.n_outputs(),
            Self::Rom(r) => r.n_outputs(),
        }
    }

    pub fn evaluate(&self, inputs: &[Value]) -> Vec<Value> {
        match self {
            Self::Input(p) => p.evaluate(inputs),
            Self::Constant(c) => c.evaluate(inputs),
            Self::Output => vec![],
            Self::Gate(g) => g.evaluate(inputs),
            Self::Mux(m) => m.evaluate(inputs),
            Self::Demux(d) => d.evaluate(inputs),
            Self::Splitter(s) => s.evaluate(inputs),
            Self::Encoder(e) => e.evaluate(inputs),
            Self::Adder(a) => a.evaluate(inputs),
            Self::Subtractor(s) => s.evaluate(inputs),
            Self::Multiplier(m) => m.evaluate(inputs),
            Self::Divider(d) => d.evaluate(inputs),
            Self::Comparator(c) => c.evaluate(inputs),
            Self::Rom(r) => r.evaluate(inputs),
        }
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Input(p) => p.input_width(i),
            Self::Constant(c) => c.input_width(i),
            Self::Output => None,
            Self::Gate(g) => g.input_width(i),
            Self::Mux(m) => m.input_width(i),
            Self::Demux(d) => d.input_width(i),
            Self::Splitter(s) => s.input_width(i),
            Self::Encoder(e) => e.input_width(i),
            Self::Adder(a) => a.input_width(i),
            Self::Subtractor(s) => s.input_width(i),
            Self::Multiplier(m) => m.input_width(i),
            Self::Divider(d) => d.input_width(i),
            Self::Comparator(c) => c.input_width(i),
            Self::Rom(r) => r.input_width(i),
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Input(p) => p.output_width(i),
            Self::Constant(c) => c.output_width(i),
            Self::Output => None,
            Self::Gate(g) => g.output_width(i),
            Self::Mux(m) => m.output_width(i),
            Self::Demux(d) => d.output_width(i),
            Self::Splitter(s) => s.output_width(i),
            Self::Encoder(e) => e.output_width(i),
            Self::Adder(a) => a.output_width(i),
            Self::Subtractor(s) => s.output_width(i),
            Self::Multiplier(m) => m.output_width(i),
            Self::Divider(d) => d.output_width(i),
            Self::Comparator(c) => c.output_width(i),
            Self::Rom(r) => r.output_width(i),
        }
    }
}

pub trait SeqLogic {
    fn n_inputs(&self) -> usize;
    fn n_outputs(&self) -> usize;
    fn tick(&mut self, inputs: &[Value]) -> Vec<Value>;
    // Applies asynchronous, level-sensitive effects to latched state given the
    // current inputs - e.g. an async reset that clears the value the instant
    // it's held, with no clock edge. Run by Circuit::settle() on every
    // evaluation of the component, so it MUST be idempotent: applying it
    // repeatedly with the same inputs must leave state unchanged after the
    // first. A purely clocked component has no async effects and leaves state
    // untouched. This is the one place, besides tick(), that mutates latched
    // state - see the Circuit::settle() call site for why that's sound.
    fn apply_async(&mut self, inputs: &[Value]);
    // The latched output value(s), reported as-is without consulting inputs or
    // advancing state. Any async input effect has already been folded into the
    // latched state by apply_async(), so this stays a pure read.
    fn observe(&self) -> Vec<Value>;
    fn snapshot(&self) -> SeqState;
    // Restores the latched state to its power-on initial value (what the
    // constructor sets), without touching construction params. Drives the
    // GUI's clock "Stop" (see Circuit::reset_sequential).
    fn reset(&mut self);
    // Expected bit width of pin `i`, from construction params (not any live
    // Value). None means any width (currently only Output). Used by
    // Circuit::resolve_net() to flag width-disagreeing nets.
    fn input_width(&self, i: usize) -> Option<u8>;
    fn output_width(&self, i: usize) -> Option<u8>;
}

// LogicSeq mirrors LogicComb, except its config struct (Reg) holds only
// static params - the mutable latched `value` lives in the enum variant, not
// the struct, so a construction record can embed Reg without runtime state.
#[derive(Debug)]
pub enum LogicSeq {
    Reg(Reg),
    ShiftReg(ShiftReg),
    DFlipFlop(DFlipFlop),
    TFlipFlop(TFlipFlop),
    JKFlipFlop(JKFlipFlop),
    SRFlipFlop(SRFlipFlop),
    Counter(Counter),
    Ram(RamCell),
}

// Generic reflection of LogicSeq's persisted state - one arm per LogicSeq
// variant, colocated here for the same "new variant -> matching arm"
// locality the tick/observe dispatch above already relies on.
#[derive(Debug, Clone)]
pub enum SeqState {
    Reg(Value),
    ShiftReg(Vec<Value>),
    FlipFlop(Value),
    Counter { value: Value, carry: Value },
    Ram(Value),
}

impl LogicSeq {
    pub fn n_inputs(&self) -> usize {
        match self {
            Self::Reg(reg) => reg.n_inputs(),
            Self::ShiftReg(sr) => sr.n_inputs(),
            Self::DFlipFlop(ff) => ff.n_inputs(),
            Self::TFlipFlop(ff) => ff.n_inputs(),
            Self::JKFlipFlop(ff) => ff.n_inputs(),
            Self::SRFlipFlop(ff) => ff.n_inputs(),
            Self::Counter(c) => c.n_inputs(),
            Self::Ram(r) => r.n_inputs(),
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Self::Reg(reg) => reg.n_outputs(),
            Self::ShiftReg(sr) => sr.n_outputs(),
            Self::DFlipFlop(ff) => ff.n_outputs(),
            Self::TFlipFlop(ff) => ff.n_outputs(),
            Self::JKFlipFlop(ff) => ff.n_outputs(),
            Self::SRFlipFlop(ff) => ff.n_outputs(),
            Self::Counter(c) => c.n_outputs(),
            Self::Ram(r) => r.n_outputs(),
        }
    }

    pub fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        match self {
            LogicSeq::Reg(reg) => reg.tick(inputs),
            Self::ShiftReg(sr) => sr.tick(inputs),
            Self::DFlipFlop(ff) => ff.tick(inputs),
            Self::TFlipFlop(ff) => ff.tick(inputs),
            Self::JKFlipFlop(ff) => ff.tick(inputs),
            Self::SRFlipFlop(ff) => ff.tick(inputs),
            Self::Counter(c) => c.tick(inputs),
            Self::Ram(r) => r.tick(inputs),
        }
    }

    pub fn apply_async(&mut self, inputs: &[Value]) {
        match self {
            Self::Reg(reg) => reg.apply_async(inputs),
            Self::ShiftReg(sr) => sr.apply_async(inputs),
            Self::DFlipFlop(ff) => ff.apply_async(inputs),
            Self::TFlipFlop(ff) => ff.apply_async(inputs),
            Self::JKFlipFlop(ff) => ff.apply_async(inputs),
            Self::SRFlipFlop(ff) => ff.apply_async(inputs),
            Self::Counter(c) => c.apply_async(inputs),
            Self::Ram(r) => r.apply_async(inputs),
        }
    }

    pub fn observe(&self) -> Vec<Value> {
        match self {
            Self::Reg(reg) => reg.observe(),
            Self::ShiftReg(sr) => sr.observe(),
            Self::DFlipFlop(ff) => ff.observe(),
            Self::TFlipFlop(ff) => ff.observe(),
            Self::JKFlipFlop(ff) => ff.observe(),
            Self::SRFlipFlop(ff) => ff.observe(),
            Self::Counter(c) => c.observe(),
            Self::Ram(r) => r.observe(),
        }
    }

    pub fn reset(&mut self) {
        match self {
            Self::Reg(reg) => reg.reset(),
            Self::ShiftReg(sr) => sr.reset(),
            Self::DFlipFlop(ff) => ff.reset(),
            Self::TFlipFlop(ff) => ff.reset(),
            Self::JKFlipFlop(ff) => ff.reset(),
            Self::SRFlipFlop(ff) => ff.reset(),
            Self::Counter(c) => c.reset(),
            Self::Ram(r) => r.reset(),
        }
    }

    // A Clone-able snapshot of this variant's persisted (non-input-derived)
    // state, independent of evaluate()/observe()'s input-driven output. Used
    // to capture a sequential component's state before tick_clock() mutates
    // it, so undo can restore it directly later.
    pub(crate) fn snapshot(&self) -> SeqState {
        match self {
            Self::Reg(reg) => reg.snapshot(),
            Self::ShiftReg(sr) => sr.snapshot(),
            Self::DFlipFlop(ff) => ff.snapshot(),
            Self::TFlipFlop(ff) => ff.snapshot(),
            Self::JKFlipFlop(ff) => ff.snapshot(),
            Self::SRFlipFlop(ff) => ff.snapshot(),
            Self::Counter(c) => c.snapshot(),
            Self::Ram(r) => r.snapshot(),
        }
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Reg(reg) => reg.input_width(i),
            Self::ShiftReg(sr) => sr.input_width(i),
            Self::DFlipFlop(ff) => ff.input_width(i),
            Self::TFlipFlop(ff) => ff.input_width(i),
            Self::JKFlipFlop(ff) => ff.input_width(i),
            Self::SRFlipFlop(ff) => ff.input_width(i),
            Self::Counter(c) => c.input_width(i),
            Self::Ram(r) => r.input_width(i),
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match self {
            Self::Reg(reg) => reg.output_width(i),
            Self::ShiftReg(sr) => sr.output_width(i),
            Self::DFlipFlop(ff) => ff.output_width(i),
            Self::TFlipFlop(ff) => ff.output_width(i),
            Self::JKFlipFlop(ff) => ff.output_width(i),
            Self::SRFlipFlop(ff) => ff.output_width(i),
            Self::Counter(c) => c.output_width(i),
            Self::Ram(r) => r.output_width(i),
        }
    }
}

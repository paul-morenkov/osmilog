use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OverflowAction {
    #[default]
    Wrap,
    StayMax,
    PassMax,
    LoadNext,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CounterConf {
    pub data_width: u8,
    pub max_value: u32,
    pub overflow_action: OverflowAction,
}

impl CounterConf {
    pub const DATA_PIN: usize = 0;
    pub const LOAD_PIN: usize = 1;
    pub const COUNT_PIN: usize = 2;

    pub const Q_PIN: usize = 0;
    pub const CARRY_PIN: usize = 1;
}

impl CounterConf {
    pub fn n_inputs(&self) -> usize {
        3
    }

    pub fn n_outputs(&self) -> usize {
        2
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            CounterConf::DATA_PIN => Some(self.data_width),
            CounterConf::LOAD_PIN => Some(1),
            CounterConf::COUNT_PIN => Some(1),
            _ => None,
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            CounterConf::Q_PIN => Some(self.data_width),
            CounterConf::CARRY_PIN => Some(1),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Counter {
    conf: CounterConf,
    value: Value,
    // Latched alongside value, not recomputed live - SeqLogic::observe() has
    // no access to current inputs, so carry must be captured at tick() time
    // like Q rather than derived on read.
    carry: Value,
}

impl Counter {
    pub fn new(data_width: u8, max_value: u32, overflow_action: OverflowAction) -> Self {
        Self {
            conf: CounterConf {
                data_width,
                max_value,
                overflow_action,
            },
            value: Value::new(0, data_width),
            carry: Value::ZERO,
        }
    }

    // Bits of `v` if it's a Fixed value of exactly `width` bits, else None -
    // an unexpected width (or Floating/Invalid) makes the step's result
    // Floating, mirroring how CombLogic ops treat width-mismatched operands.
    fn bits_of(v: Value, width: u8) -> Option<u32> {
        match v {
            Value::Fixed { bits, width: w } if w == width => Some(bits),
            _ => None,
        }
    }

    // count=1, load=0: increments, applying overflow_action once the
    // pre-step value is at or above max. `carry` reflects that pre-step
    // condition regardless of which overflow_action is configured.
    fn step_up(&self, data: Value) -> (Value, Value) {
        let width = self.conf.data_width;
        let mask = Value::mask(width);
        let max = self.conf.max_value & mask;
        let Some(bits) = Self::bits_of(self.value, width) else {
            return (Value::Floating, Value::Floating);
        };

        let at_max = bits >= max;
        let carry = if at_max { Value::ONE } else { Value::ZERO };
        let new_value = if !at_max {
            Value::new(bits.wrapping_add(1) & mask, width)
        } else {
            match self.conf.overflow_action {
                OverflowAction::Wrap => Value::new(0, width),
                OverflowAction::StayMax => Value::new(max, width),
                // Ignores max_value; only the natural data_width bit range wraps.
                OverflowAction::PassMax => Value::new(bits.wrapping_add(1) & mask, width),
                OverflowAction::LoadNext => data,
            }
        };
        (new_value, carry)
    }

    // count=1, load=1: decrements, mirroring step_up at the zero bound.
    fn step_down(&self, data: Value) -> (Value, Value) {
        let width = self.conf.data_width;
        let mask = Value::mask(width);
        let max = self.conf.max_value & mask;
        let Some(bits) = Self::bits_of(self.value, width) else {
            return (Value::Floating, Value::Floating);
        };

        let at_zero = bits == 0;
        let carry = if at_zero { Value::ONE } else { Value::ZERO };
        let new_value = if !at_zero {
            Value::new(bits.wrapping_sub(1) & mask, width)
        } else {
            match self.conf.overflow_action {
                OverflowAction::Wrap => Value::new(max, width),
                OverflowAction::StayMax => Value::new(0, width),
                // Ignores max_value; wraps to the natural data_width bit range (all ones).
                OverflowAction::PassMax => Value::new(bits.wrapping_sub(1) & mask, width),
                OverflowAction::LoadNext => data,
            }
        };
        (new_value, carry)
    }
}

impl SeqLogic for Counter {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        let data = inputs[CounterConf::DATA_PIN];
        let load = inputs[CounterConf::LOAD_PIN];
        let count = inputs[CounterConf::COUNT_PIN];

        let (new_value, carry) = match (load, count) {
            (Value::ZERO, Value::ZERO) => (self.value, Value::ZERO), // hold
            (Value::ONE, Value::ZERO) => (data, Value::ZERO),        // load
            (Value::ZERO, Value::ONE) => self.step_up(data),         // increment
            (Value::ONE, Value::ONE) => self.step_down(data),        // decrement
            _ => (Value::Floating, Value::Floating),
        };

        self.value = new_value;
        self.carry = carry;
        vec![self.value, self.carry]
    }

    // The counter has no asynchronous inputs, so there's nothing to apply
    // during settle - its state only changes on a clock tick.
    fn apply_async(&mut self, _inputs: &[Value]) {}

    fn observe(&self) -> Vec<Value> {
        vec![self.value, self.carry]
    }

    fn reset(&mut self) {
        self.value = Value::new(0, self.conf.data_width);
        self.carry = Value::ZERO;
    }

    fn snapshot(&self) -> SeqState {
        SeqState::Counter {
            value: self.value,
            carry: self.carry,
        }
    }

    fn input_width(&self, i: usize) -> Option<u8> {
        self.conf.input_width(i)
    }

    fn output_width(&self, i: usize) -> Option<u8> {
        self.conf.output_width(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::component::LogicSeq;
    use test_case::test_case;

    fn new_counter(data_width: u8, max_value: u32, overflow_action: OverflowAction) -> LogicSeq {
        LogicSeq::Counter(Counter::new(data_width, max_value, overflow_action))
    }

    fn tick(seq: &mut LogicSeq, data: Value, load: Value, count: Value) -> Vec<Value> {
        seq.tick(&[data, load, count])
    }

    #[test]
    fn test_initial_value_before_any_tick() {
        let c = new_counter(4, 15, OverflowAction::Wrap);
        assert_eq!(c.observe(), vec![Value::new(0, 4), Value::ZERO]);
    }

    #[test]
    fn test_hold() {
        let mut c = new_counter(4, 15, OverflowAction::Wrap);
        assert_eq!(
            tick(&mut c, Value::new(9, 4), Value::ZERO, Value::ZERO),
            vec![Value::new(0, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_load() {
        let mut c = new_counter(4, 15, OverflowAction::Wrap);
        assert_eq!(
            tick(&mut c, Value::new(7, 4), Value::ONE, Value::ZERO),
            vec![Value::new(7, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_increment() {
        let mut c = new_counter(4, 15, OverflowAction::Wrap);
        tick(&mut c, Value::new(0, 4), Value::ONE, Value::ZERO); // load 0
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ZERO, Value::ONE),
            vec![Value::new(1, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_decrement() {
        let mut c = new_counter(4, 15, OverflowAction::Wrap);
        tick(&mut c, Value::new(5, 4), Value::ONE, Value::ZERO); // load 5
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ONE, Value::ONE),
            vec![Value::new(4, 4), Value::ZERO]
        );
    }

    #[test_case(OverflowAction::Wrap, 0 ; "wrap goes to 0")]
    #[test_case(OverflowAction::StayMax, 9 ; "stay_max saturates at max")]
    #[test_case(OverflowAction::PassMax, 10 ; "pass_max keeps counting past max")]
    fn test_increment_overflow(action: OverflowAction, expect: u32) {
        let mut c = new_counter(4, 9, action);
        tick(&mut c, Value::new(9, 4), Value::ONE, Value::ZERO); // load max (9)
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ZERO, Value::ONE),
            vec![Value::new(expect, 4), Value::ONE]
        );
    }

    #[test]
    fn test_increment_overflow_load_next() {
        let mut c = new_counter(4, 9, OverflowAction::LoadNext);
        tick(&mut c, Value::new(9, 4), Value::ONE, Value::ZERO); // load max (9)
        assert_eq!(
            tick(&mut c, Value::new(3, 4), Value::ZERO, Value::ONE),
            vec![Value::new(3, 4), Value::ONE]
        );
    }

    #[test]
    fn test_pass_max_wraps_at_natural_bit_width() {
        // 4-bit counter, max_value 9: PassMax ignores max_value and only
        // wraps once the natural 4-bit range (15) rolls over.
        let mut c = new_counter(4, 9, OverflowAction::PassMax);
        tick(&mut c, Value::new(15, 4), Value::ONE, Value::ZERO); // load 15 (natural max)
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ZERO, Value::ONE),
            vec![Value::new(0, 4), Value::ONE]
        );
    }

    #[test_case(OverflowAction::Wrap, 9 ; "wrap goes to max")]
    #[test_case(OverflowAction::StayMax, 0 ; "stay_max saturates at 0")]
    #[test_case(OverflowAction::PassMax, 15 ; "pass_max wraps via natural bit width")]
    fn test_decrement_underflow(action: OverflowAction, expect: u32) {
        let mut c = new_counter(4, 9, action);
        tick(&mut c, Value::new(0, 4), Value::ONE, Value::ZERO); // load 0
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ONE, Value::ONE),
            vec![Value::new(expect, 4), Value::ONE]
        );
    }

    #[test]
    fn test_decrement_underflow_load_next() {
        let mut c = new_counter(4, 9, OverflowAction::LoadNext);
        tick(&mut c, Value::new(0, 4), Value::ONE, Value::ZERO); // load 0
        assert_eq!(
            tick(&mut c, Value::new(6, 4), Value::ONE, Value::ONE),
            vec![Value::new(6, 4), Value::ONE]
        );
    }

    #[test_case(Value::Floating, Value::ZERO ; "load floating")]
    #[test_case(Value::ZERO, Value::Floating ; "count floating")]
    #[test_case(Value::new(1, 2), Value::ZERO ; "load wrong width")]
    fn test_invalid_control_combo_floats_and_corrupts_value(load: Value, count: Value) {
        let mut c = new_counter(4, 15, OverflowAction::Wrap);
        tick(&mut c, Value::new(5, 4), Value::ONE, Value::ZERO); // load 5
        assert_eq!(
            tick(&mut c, Value::new(1, 4), load, count),
            vec![Value::Floating, Value::Floating]
        );
        // Value stays corrupted (Floating) until a fresh load recovers it,
        // mirroring SRFlipFlop's forbidden-state behavior.
        assert_eq!(c.observe(), vec![Value::Floating, Value::Floating]);
        assert_eq!(
            tick(&mut c, Value::new(2, 4), Value::ONE, Value::ZERO),
            vec![Value::new(2, 4), Value::ZERO]
        );
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut c = new_counter(4, 15, OverflowAction::Wrap);

        // load 3
        assert_eq!(
            tick(&mut c, Value::new(3, 4), Value::ONE, Value::ZERO),
            vec![Value::new(3, 4), Value::ZERO]
        );
        // increment -> 4
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ZERO, Value::ONE),
            vec![Value::new(4, 4), Value::ZERO]
        );
        // hold -> 4
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ZERO, Value::ZERO),
            vec![Value::new(4, 4), Value::ZERO]
        );
        // decrement -> 3
        assert_eq!(
            tick(&mut c, Value::Floating, Value::ONE, Value::ONE),
            vec![Value::new(3, 4), Value::ZERO]
        );
    }
}

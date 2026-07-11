use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

/// A D flip-flop is essentially a single-bit register.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DFlipFlopConf;

impl DFlipFlopConf {
    pub const DATA_PIN: usize = 0;
    pub const WRITE_EN_PIN: usize = 1;
    // Asynchronous reset: forces Q to zero the instant it's held (via
    // observe), and clears the latched value on the next tick so the reset
    // sticks. Active only on exactly Value::ONE.
    pub const RESET_PIN: usize = 2;
}

impl DFlipFlopConf {
    pub fn n_inputs(&self) -> usize {
        3
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            DFlipFlopConf::DATA_PIN => Some(1),     // data
            DFlipFlopConf::WRITE_EN_PIN => Some(1), // write_enable
            DFlipFlopConf::RESET_PIN => Some(1),    // async reset
            _ => None,
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            0 => Some(1),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct DFlipFlop {
    conf: DFlipFlopConf,
    value: Value,
}

impl DFlipFlop {
    pub fn new() -> Self {
        Self {
            conf: DFlipFlopConf,
            value: Value::ZERO,
        }
    }
}

impl Default for DFlipFlop {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqLogic for DFlipFlop {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        // Async reset dominates the clocked write and destroys the latched
        // value so the clear persists after the reset pin is released.
        if matches!(inputs[DFlipFlopConf::RESET_PIN], Value::ONE) {
            self.value = Value::ZERO;
        } else {
            let data = inputs[DFlipFlopConf::DATA_PIN];
            let write_enable = inputs[DFlipFlopConf::WRITE_EN_PIN];
            if matches!(write_enable, Value::ONE | Value::Floating) {
                self.value = data;
            }
        }
        vec![self.value]
    }

    fn apply_async(&mut self, inputs: &[Value]) {
        // Async reset: while held (exactly ONE) it destroys the latched value,
        // so the clear takes effect during settle() with no clock tick and
        // persists after the pin is released. Idempotent.
        if matches!(inputs[DFlipFlopConf::RESET_PIN], Value::ONE) {
            self.value = Value::ZERO;
        }
    }

    fn observe(&self) -> Vec<Value> {
        vec![self.value]
    }

    fn reset(&mut self) {
        self.value = Value::ZERO;
    }

    fn snapshot(&self) -> SeqState {
        SeqState::FlipFlop(self.value)
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
    use crate::sim::value::Value;
    use test_case::test_case;

    fn new_d_flip_flop() -> LogicSeq {
        LogicSeq::DFlipFlop(DFlipFlop::new())
    }

    // No async reset asserted (the common case in these tests).
    const NO_RST: Value = Value::ZERO;

    #[test]
    fn test_initial_value_before_any_tick() {
        let ff = new_d_flip_flop();
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test_case(Value::ONE; "write_enable exactly one")]
    #[test_case(Value::Floating; "write_enable floating")]
    fn test_latches_on_write_enable_holds_otherwise(we: Value) {
        let mut ff = new_d_flip_flop();
        // Zero-initialized, unaffected by data already present pre-tick.
        assert_eq!(ff.observe(), vec![Value::ZERO]);

        // write_enable=1, tick: latches data.
        assert_eq!(ff.tick(&[Value::ONE, we, NO_RST]), vec![Value::ONE]);

        // write_enable=0, data changes, tick: holds previous value.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ZERO, NO_RST]),
            vec![Value::ONE]
        );
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_latching_cases(we: Value) {
        let mut ff = new_d_flip_flop();
        assert_eq!(ff.tick(&[Value::ONE, we, NO_RST]), vec![Value::ZERO]);
    }

    #[test]
    fn test_apply_async_reset_clears_state_destructively() {
        let mut ff = new_d_flip_flop();
        ff.tick(&[Value::ONE, Value::ONE, NO_RST]); // latch 1
                                                    // Reset held: apply_async clears Q, no tick.
        ff.apply_async(&[Value::Floating, Value::Floating, Value::ONE]);
        assert_eq!(ff.observe(), vec![Value::ZERO]);
        // Reset released: stays 0 (destroyed, not restored).
        ff.apply_async(&[Value::ONE, Value::ONE, Value::ZERO]);
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test]
    fn test_async_reset_dominates_on_tick() {
        let mut ff = new_d_flip_flop();
        ff.tick(&[Value::ONE, Value::ONE, NO_RST]); // latch 1
                                                    // reset=1 dominates write_enable=1/data=1: latches 0.
        assert_eq!(ff.tick(&[Value::ONE, Value::ONE, Value::ONE]), vec![Value::ZERO]);
    }

    #[test_case(Value::ZERO; "reset exactly zero")]
    #[test_case(Value::Floating; "reset floating")]
    #[test_case(Value::new(1, 2); "reset wrong width")]
    fn test_reset_only_activates_on_exactly_one(rst: Value) {
        let mut ff = new_d_flip_flop();
        ff.tick(&[Value::ONE, Value::ONE, NO_RST]); // latch 1
                                                    // reset not exactly ONE: apply_async leaves state alone.
        ff.apply_async(&[Value::ZERO, Value::ONE, rst]);
        assert_eq!(ff.observe(), vec![Value::ONE]);
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut ff = new_d_flip_flop();

        // tick 1: we=1, data=1 -> latches 1.
        assert_eq!(ff.tick(&[Value::ONE, Value::ONE, NO_RST]), vec![Value::ONE]);

        // tick 2: we=0, data=0 -> holds 1.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ZERO, NO_RST]),
            vec![Value::ONE]
        );

        // tick 3: we=1, data=0 -> latches 0.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ZERO]
        );
    }
}

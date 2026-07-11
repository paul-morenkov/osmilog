use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

/// A D flip-flop is essentially a single-bit register.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TFlipFlopConf;

impl TFlipFlopConf {
    pub const TOGGLE_PIN: usize = 0;
    pub const WRITE_EN_PIN: usize = 1;
    // Asynchronous reset: forces Q to zero the instant it's held (via
    // observe), and clears the latched value on the next tick so the reset
    // sticks. Active only on exactly Value::ONE.
    pub const RESET_PIN: usize = 2;
}

impl TFlipFlopConf {
    pub fn n_inputs(&self) -> usize {
        3
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            TFlipFlopConf::TOGGLE_PIN => Some(1),   // toggle
            TFlipFlopConf::WRITE_EN_PIN => Some(1), // write_enable
            TFlipFlopConf::RESET_PIN => Some(1),    // async reset
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
pub struct TFlipFlop {
    conf: TFlipFlopConf,
    value: Value,
}

impl TFlipFlop {
    pub fn new() -> Self {
        Self {
            conf: TFlipFlopConf,
            value: Value::ZERO,
        }
    }
}

impl Default for TFlipFlop {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqLogic for TFlipFlop {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        // Async reset dominates the clocked toggle and destroys the latched
        // value so the clear persists after the reset pin is released.
        if matches!(inputs[TFlipFlopConf::RESET_PIN], Value::ONE) {
            self.value = Value::ZERO;
        } else {
            // FIXME: Should all of these be testing for 1 bit width?
            let toggle = inputs[TFlipFlopConf::TOGGLE_PIN] == Value::ONE;
            let write_enable = inputs[TFlipFlopConf::WRITE_EN_PIN];

            if matches!(write_enable, Value::ONE | Value::Floating) && toggle {
                self.value = !self.value
            }
        }
        vec![self.value]
    }

    fn apply_async(&mut self, inputs: &[Value]) {
        // Async reset: while held (exactly ONE) it destroys the latched value,
        // so the clear takes effect during settle() with no clock tick and
        // persists after the pin is released. Idempotent.
        if matches!(inputs[TFlipFlopConf::RESET_PIN], Value::ONE) {
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

    fn new_t_flip_flop() -> LogicSeq {
        LogicSeq::TFlipFlop(TFlipFlop::new())
    }

    // No async reset asserted (the common case in these tests).
    const NO_RST: Value = Value::ZERO;

    #[test]
    fn test_initial_value_before_any_tick() {
        let ff = new_t_flip_flop();
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test_case(Value::ONE; "write_enable exactly one")]
    #[test_case(Value::Floating; "write_enable floating")]
    fn test_toggles_on_write_enable_holds_otherwise(we: Value) {
        let mut ff = new_t_flip_flop();
        // Zero-initialized, unaffected by toggle already present pre-tick.
        assert_eq!(ff.observe(), vec![Value::ZERO]);

        // toggle=1, write_enable=1, tick: flips 0 -> 1.
        assert_eq!(ff.tick(&[Value::ONE, we, NO_RST]), vec![Value::ONE]);

        // write_enable=0, toggle still 1, tick: holds previous value.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, NO_RST]),
            vec![Value::ONE]
        );
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_toggling_cases(we: Value) {
        let mut ff = new_t_flip_flop();
        assert_eq!(ff.tick(&[Value::ONE, we, NO_RST]), vec![Value::ZERO]);
    }

    #[test_case(Value::Floating; "toggle floating")]
    #[test_case(Value::ZERO; "toggle exactly zero")]
    #[test_case(Value::new(1, 2); "toggle wrong width (bits=1, width=2)")]
    fn test_toggle_must_be_exactly_one(toggle: Value) {
        let mut ff = new_t_flip_flop();
        // write_enable=1, but toggle isn't exactly Value::ONE: holds.
        assert_eq!(ff.tick(&[toggle, Value::ONE, NO_RST]), vec![Value::ZERO]);
    }

    #[test]
    fn test_apply_async_reset_clears_state_destructively() {
        // Mirrors the intended GUI flow: toggle up to 1, then pulse the async
        // reset pin - state clears at once, no tick, and stays cleared.
        let mut ff = new_t_flip_flop();
        ff.tick(&[Value::ONE, Value::ONE, NO_RST]); // toggle to 1
        ff.apply_async(&[Value::Floating, Value::Floating, Value::ONE]);
        assert_eq!(ff.observe(), vec![Value::ZERO]);
        // Reset released: stays 0 (destroyed, not restored).
        ff.apply_async(&[Value::ONE, Value::ONE, Value::ZERO]);
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test]
    fn test_async_reset_dominates_on_tick() {
        let mut ff = new_t_flip_flop();
        ff.tick(&[Value::ONE, Value::ONE, NO_RST]); // toggle to 1
                                                    // reset=1 dominates toggle=1/we=1: forces 0.
        assert_eq!(ff.tick(&[Value::ONE, Value::ONE, Value::ONE]), vec![Value::ZERO]);
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut ff = new_t_flip_flop();

        // tick 1: toggle=1, we=1 -> flips 0 -> 1.
        assert_eq!(ff.tick(&[Value::ONE, Value::ONE, NO_RST]), vec![Value::ONE]);

        // tick 2: toggle=0, we=1 -> holds 1 (toggle low).
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ONE]
        );

        // tick 3: toggle=1, we=0 -> holds 1 (write disabled).
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, NO_RST]),
            vec![Value::ONE]
        );

        // tick 4: toggle=1, we=1 -> flips 1 -> 0.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ONE, NO_RST]),
            vec![Value::ZERO]
        );
    }
}

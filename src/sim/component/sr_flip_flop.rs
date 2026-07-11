use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

/// An S-R flip-flop: S=R=0 holds, S=1/R=0 sets, S=0/R=1 resets, S=R=1 is the
/// forbidden state and floats the output.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SRFlipFlopConf;

impl SRFlipFlopConf {
    pub const S_PIN: usize = 0;
    pub const R_PIN: usize = 1;
    pub const WRITE_EN_PIN: usize = 2;
    // Asynchronous reset: forces Q to zero the instant it's held (via
    // observe), and clears the latched value on the next tick so the reset
    // sticks. Distinct from the clocked R input; active only on exactly
    // Value::ONE.
    pub const RESET_PIN: usize = 3;
}

impl SRFlipFlopConf {
    pub fn n_inputs(&self) -> usize {
        4
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            SRFlipFlopConf::S_PIN => Some(1),
            SRFlipFlopConf::R_PIN => Some(1),
            SRFlipFlopConf::WRITE_EN_PIN => Some(1),
            SRFlipFlopConf::RESET_PIN => Some(1),
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
pub struct SRFlipFlop {
    conf: SRFlipFlopConf,
    value: Value,
}

impl SRFlipFlop {
    pub fn new() -> Self {
        Self {
            conf: SRFlipFlopConf,
            value: Value::ZERO,
        }
    }
}

impl Default for SRFlipFlop {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqLogic for SRFlipFlop {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        // Async reset dominates the clocked S-R logic and destroys the latched
        // value so the clear persists after the reset pin is released.
        if matches!(inputs[SRFlipFlopConf::RESET_PIN], Value::ONE) {
            self.value = Value::ZERO;
        } else {
            let s = inputs[SRFlipFlopConf::S_PIN];
            let r = inputs[SRFlipFlopConf::R_PIN];
            let write_enable = inputs[SRFlipFlopConf::WRITE_EN_PIN];

            if matches!(write_enable, Value::ONE | Value::Floating) {
                self.value = match (s, r) {
                    (Value::ZERO, Value::ZERO) => self.value, // hold
                    (Value::ZERO, Value::ONE) => Value::ZERO, // reset
                    (Value::ONE, Value::ZERO) => Value::ONE,  // set
                    _ => Value::Floating,                     // forbidden state
                };
            }
        }
        vec![self.value]
    }

    fn apply_async(&mut self, inputs: &[Value]) {
        // Async reset: while held (exactly ONE) it destroys the latched value,
        // so the clear takes effect during settle() with no clock tick and
        // persists after the pin is released. Distinct from the clocked R
        // input, which only acts on a tick. Idempotent.
        if matches!(inputs[SRFlipFlopConf::RESET_PIN], Value::ONE) {
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

    fn new_sr_flip_flop() -> LogicSeq {
        LogicSeq::SRFlipFlop(SRFlipFlop::new())
    }

    // No async reset asserted (the common case in these tests).
    const NO_RST: Value = Value::ZERO;

    #[test]
    fn test_initial_value_before_any_tick() {
        let ff = new_sr_flip_flop();
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test_case(Value::ONE; "write_enable exactly one")]
    #[test_case(Value::Floating; "write_enable floating")]
    fn test_set_and_reset(we: Value) {
        let mut ff = new_sr_flip_flop();

        // S=1, R=0: sets.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, we, NO_RST]),
            vec![Value::ONE]
        );

        // S=0, R=1: resets.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ONE, we, NO_RST]),
            vec![Value::ZERO]
        );
    }

    #[test]
    fn test_hold_when_s_and_r_both_zero() {
        let mut ff = new_sr_flip_flop();
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ONE]
        );
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ONE]
        );
    }

    #[test]
    fn test_forbidden_state_floats_output() {
        let mut ff = new_sr_flip_flop();
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ONE]
        );

        // S=1, R=1: forbidden -> floats.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ONE, Value::ONE, NO_RST]),
            vec![Value::Floating]
        );
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_latching_cases(we: Value) {
        let mut ff = new_sr_flip_flop();
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, we, NO_RST]),
            vec![Value::ZERO]
        );
    }

    #[test]
    fn test_apply_async_reset_clears_state_destructively() {
        let mut ff = new_sr_flip_flop();
        ff.tick(&[Value::ONE, Value::ZERO, Value::ONE, NO_RST]); // set to 1
                                                                // Reset held: apply_async clears Q, no tick.
        ff.apply_async(&[Value::Floating, Value::Floating, Value::Floating, Value::ONE]);
        assert_eq!(ff.observe(), vec![Value::ZERO]);
        // Reset released: stays 0 (destroyed, not restored).
        ff.apply_async(&[Value::ONE, Value::ZERO, Value::ONE, Value::ZERO]);
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test]
    fn test_async_reset_dominates_on_tick() {
        let mut ff = new_sr_flip_flop();
        ff.tick(&[Value::ONE, Value::ZERO, Value::ONE, NO_RST]); // set to 1
                                                                // reset=1 dominates S=1/R=0/we=1.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, Value::ONE, Value::ONE]),
            vec![Value::ZERO]
        );
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut ff = new_sr_flip_flop();

        // tick 1: S=1, R=0, we=1 -> sets 1.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ONE]
        );

        // tick 2: S=0, R=0, we=1 -> holds 1.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ZERO, Value::ONE, NO_RST]),
            vec![Value::ONE]
        );

        // tick 3: S=0, R=1, we=0 -> write disabled, holds 1.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ONE, Value::ZERO, NO_RST]),
            vec![Value::ONE]
        );

        // tick 4: S=0, R=1, we=1 -> resets 0.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ONE, Value::ONE, NO_RST]),
            vec![Value::ZERO]
        );
    }
}

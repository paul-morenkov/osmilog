use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

/// A J-K flip-flop: J=K=0 holds, J=1/K=0 sets, J=0/K=1 resets, J=K=1 toggles.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JKFlipFlopConf;

impl JKFlipFlopConf {
    pub const J_PIN: usize = 0;
    pub const K_PIN: usize = 1;
    pub const WRITE_EN_PIN: usize = 2;
}

impl JKFlipFlopConf {
    pub fn n_inputs(&self) -> usize {
        3
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            JKFlipFlopConf::J_PIN => Some(1),
            JKFlipFlopConf::K_PIN => Some(1),
            JKFlipFlopConf::WRITE_EN_PIN => Some(1),
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
pub struct JKFlipFlop {
    conf: JKFlipFlopConf,
    value: Value,
}

impl JKFlipFlop {
    pub fn new() -> Self {
        Self {
            conf: JKFlipFlopConf,
            value: Value::ZERO,
        }
    }
}

impl Default for JKFlipFlop {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqLogic for JKFlipFlop {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        let j = inputs[JKFlipFlopConf::J_PIN];
        let k = inputs[JKFlipFlopConf::K_PIN];
        let write_enable = inputs[JKFlipFlopConf::WRITE_EN_PIN];

        if matches!(write_enable, Value::ONE | Value::Floating) {
            self.value = match (j, k) {
                (Value::ZERO, Value::ZERO) => self.value, // hold
                (Value::ZERO, Value::ONE) => Value::ZERO, // reset
                (Value::ONE, Value::ZERO) => Value::ONE,  // set
                (Value::ONE, Value::ONE) => !self.value,  // toggle
                _ => Value::Floating,
            };
        }
        vec![self.value]
    }

    fn observe(&self) -> Vec<Value> {
        vec![self.value]
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

    fn new_jk_flip_flop() -> LogicSeq {
        LogicSeq::JKFlipFlop(JKFlipFlop::new())
    }

    #[test]
    fn test_initial_value_before_any_tick() {
        let ff = new_jk_flip_flop();
        assert_eq!(ff.observe(), vec![Value::ZERO]);
    }

    #[test_case(Value::ONE; "write_enable exactly one")]
    #[test_case(Value::Floating; "write_enable floating")]
    fn test_set_and_reset(we: Value) {
        let mut ff = new_jk_flip_flop();

        // J=1, K=0: sets.
        assert_eq!(ff.tick(&[Value::ONE, Value::ZERO, we]), vec![Value::ONE]);

        // J=0, K=1: resets.
        assert_eq!(ff.tick(&[Value::ZERO, Value::ONE, we]), vec![Value::ZERO]);
    }

    #[test]
    fn test_hold_when_j_and_k_both_zero() {
        let mut ff = new_jk_flip_flop();
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, Value::ONE]),
            vec![Value::ONE]
        );
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ZERO, Value::ONE]),
            vec![Value::ONE]
        );
    }

    #[test]
    fn test_toggle_when_j_and_k_both_one() {
        let mut ff = new_jk_flip_flop();
        assert_eq!(ff.observe(), vec![Value::ZERO]);

        // J=1, K=1: toggles 0 -> 1.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ONE, Value::ONE]),
            vec![Value::ONE]
        );

        // J=1, K=1: toggles 1 -> 0.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ONE, Value::ONE]),
            vec![Value::ZERO]
        );
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_latching_cases(we: Value) {
        let mut ff = new_jk_flip_flop();
        assert_eq!(ff.tick(&[Value::ONE, Value::ZERO, we]), vec![Value::ZERO]);
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut ff = new_jk_flip_flop();

        // tick 1: J=1, K=0, we=1 -> sets 1.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ZERO, Value::ONE]),
            vec![Value::ONE]
        );

        // tick 2: J=0, K=0, we=1 -> holds 1.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ZERO, Value::ONE]),
            vec![Value::ONE]
        );

        // tick 3: J=1, K=1, we=1 -> toggles to 0.
        assert_eq!(
            ff.tick(&[Value::ONE, Value::ONE, Value::ONE]),
            vec![Value::ZERO]
        );

        // tick 4: J=0, K=1, we=0 -> write disabled, holds 0.
        assert_eq!(
            ff.tick(&[Value::ZERO, Value::ONE, Value::ZERO]),
            vec![Value::ZERO]
        );
    }
}

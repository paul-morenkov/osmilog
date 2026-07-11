use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

/// A D flip-flop is essentially a single-bit register.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DFlipFlopConf;

impl DFlipFlopConf {
    pub const DATA_PIN: usize = 0;
    pub const WRITE_EN_PIN: usize = 1;
}

impl DFlipFlopConf {
    pub fn n_inputs(&self) -> usize {
        2
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            DFlipFlopConf::DATA_PIN => Some(1),     // data
            DFlipFlopConf::WRITE_EN_PIN => Some(1), // write_enable
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
        let data = inputs[DFlipFlopConf::DATA_PIN];
        let write_enable = inputs[DFlipFlopConf::WRITE_EN_PIN];
        if matches!(write_enable, Value::ONE | Value::Floating) {
            self.value = data;
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

    fn new_d_flip_flop() -> LogicSeq {
        LogicSeq::DFlipFlop(DFlipFlop::new())
    }

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
        assert_eq!(ff.tick(&[Value::ONE, we]), vec![Value::ONE]);

        // write_enable=0, data changes, tick: holds previous value.
        assert_eq!(ff.tick(&[Value::ZERO, Value::ZERO]), vec![Value::ONE]);
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_latching_cases(we: Value) {
        let mut ff = new_d_flip_flop();
        assert_eq!(ff.tick(&[Value::ONE, we]), vec![Value::ZERO]);
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut ff = new_d_flip_flop();

        // tick 1: we=1, data=1 -> latches 1.
        assert_eq!(ff.tick(&[Value::ONE, Value::ONE]), vec![Value::ONE]);

        // tick 2: we=0, data=0 -> holds 1.
        assert_eq!(ff.tick(&[Value::ZERO, Value::ZERO]), vec![Value::ONE]);

        // tick 3: we=1, data=0 -> latches 0.
        assert_eq!(ff.tick(&[Value::ZERO, Value::ONE]), vec![Value::ZERO]);
    }
}

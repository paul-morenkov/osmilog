use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

// Register config only - the latched runtime value lives in LogicSeq::Reg::value, not here,
// so this struct stays a pure construction record (embeddable directly in ComponentDef).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegConf {
    pub data_width: u8,
}

impl RegConf {
    pub const DATA_PIN: usize = 0;
    pub const WRITE_EN_PIN: usize = 1;
}

impl RegConf {
    pub fn n_inputs(&self) -> usize {
        2
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            RegConf::DATA_PIN => Some(self.data_width), // data
            RegConf::WRITE_EN_PIN => Some(1),           // write_enable
            _ => None,
        }
    }

    pub fn output_width(&self, i: usize) -> Option<u8> {
        match i {
            0 => Some(self.data_width),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Reg {
    conf: RegConf,
    value: Value,
}

impl Reg {
    pub fn new(data_width: u8) -> Self {
        Self {
            conf: RegConf { data_width },
            value: Value::new(0, data_width),
        }
    }
}

impl SeqLogic for Reg {
    fn n_inputs(&self) -> usize {
        self.conf.n_inputs()
    }

    fn n_outputs(&self) -> usize {
        self.conf.n_outputs()
    }

    fn tick(&mut self, inputs: &[Value]) -> Vec<Value> {
        let data = inputs[RegConf::DATA_PIN];
        let write_enable = inputs[RegConf::WRITE_EN_PIN];
        if matches!(write_enable, Value::ONE | Value::Floating) {
            self.value = data;
        }
        vec![self.value]
    }

    fn observe(&self) -> Vec<Value> {
        vec![self.value]
    }

    fn snapshot(&self) -> SeqState {
        SeqState::Reg(self.value)
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

    fn new_reg(data_width: u8) -> LogicSeq {
        LogicSeq::Reg(Reg::new(data_width))
    }

    #[test]
    fn test_initial_value_before_any_tick() {
        let reg = new_reg(4);
        assert_eq!(reg.observe(), vec![Value::new(0, 4)]);
    }

    #[test_case(Value::ONE; "write_enable exactly one")]
    #[test_case(Value::Floating; "write_enable floating")]
    fn test_latches_on_write_enable_holds_otherwise(we: Value) {
        let mut reg = new_reg(4);
        // Zero-initialized, unaffected by data already present pre-tick.
        assert_eq!(reg.observe(), vec![Value::new(0, 4)]);

        // write_enable=1, tick: latches data.
        assert_eq!(reg.tick(&[Value::new(5, 4), we]), vec![Value::new(5, 4)]);

        // write_enable=0, data changes, tick: holds previous value.
        assert_eq!(
            reg.tick(&[Value::new(9, 4), Value::ZERO]),
            vec![Value::new(5, 4)]
        );
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_latching_cases(we: Value) {
        let mut reg = new_reg(4);
        assert_eq!(reg.tick(&[Value::new(7, 4), we]), vec![Value::new(0, 4)]);
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut reg = new_reg(4);

        // tick 1: we=1, data=3 -> latches 3.
        assert_eq!(
            reg.tick(&[Value::new(3, 4), Value::ONE]),
            vec![Value::new(3, 4)]
        );

        // tick 2: we=0, data=9 -> holds 3.
        assert_eq!(
            reg.tick(&[Value::new(9, 4), Value::ZERO]),
            vec![Value::new(3, 4)]
        );

        // tick 3: we=1, data=9 -> latches 9.
        assert_eq!(
            reg.tick(&[Value::new(9, 4), Value::ONE]),
            vec![Value::new(9, 4)]
        );
    }
}

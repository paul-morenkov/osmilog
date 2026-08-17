use super::{SeqLogic, SeqState};
use crate::sim::value::Value;

// Construction record only; the latched runtime value lives in LogicSeq::Reg::value.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegConf {
    pub data_width: u8,
}

impl RegConf {
    pub const DATA_PIN: usize = 0;
    pub const WRITE_EN_PIN: usize = 1;
    // Forces the output to zero the instant it's held. Active only on
    // exactly Value::ONE.
    pub const RESET_PIN: usize = 2;
}

impl RegConf {
    pub fn n_inputs(&self) -> usize {
        3
    }

    pub fn n_outputs(&self) -> usize {
        1
    }

    pub fn input_width(&self, i: usize) -> Option<u8> {
        match i {
            RegConf::DATA_PIN => Some(self.data_width),
            RegConf::WRITE_EN_PIN => Some(1),
            RegConf::RESET_PIN => Some(1),
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
        // Async reset dominates the clocked write.
        if matches!(inputs[RegConf::RESET_PIN], Value::ONE) {
            self.value = Value::new(0, self.conf.data_width);
        } else {
            let data = inputs[RegConf::DATA_PIN];
            let write_enable = inputs[RegConf::WRITE_EN_PIN];
            if matches!(write_enable, Value::ONE | Value::Floating) {
                self.value = data;
            }
        }
        vec![self.value]
    }

    fn apply_async(&mut self, inputs: &[Value]) {
        // Clears immediately while held, so the reset takes effect during
        // settle() with no clock tick.
        if matches!(inputs[RegConf::RESET_PIN], Value::ONE) {
            self.value = Value::new(0, self.conf.data_width);
        }
    }

    fn observe(&self) -> Vec<Value> {
        vec![self.value]
    }

    fn reset(&mut self) {
        self.value = Value::new(0, self.conf.data_width);
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

    const NO_RST: Value = Value::ZERO;

    fn ins(data: Value, we: Value) -> [Value; 3] {
        [data, we, NO_RST]
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

        assert_eq!(reg.tick(&ins(Value::new(5, 4), we)), vec![Value::new(5, 4)]);

        assert_eq!(
            reg.tick(&ins(Value::new(9, 4), Value::ZERO)),
            vec![Value::new(5, 4)]
        );
    }

    #[test_case(Value::new(1, 2); "write_enable wrong width (bits=1, width=2)")]
    #[test_case(Value::ZERO; "write_enable exactly zero")]
    fn test_write_enable_non_latching_cases(we: Value) {
        let mut reg = new_reg(4);
        assert_eq!(reg.tick(&ins(Value::new(7, 4), we)), vec![Value::new(0, 4)]);
    }

    #[test]
    fn test_apply_async_reset_clears_state_destructively() {
        let mut reg = new_reg(4);
        reg.tick(&ins(Value::new(9, 4), Value::ONE));
        assert_eq!(reg.observe(), vec![Value::new(9, 4)]);

        // Reset held (exactly ONE): apply_async clears the latch, no tick.
        reg.apply_async(&[Value::Floating, Value::Floating, Value::ONE]);
        assert_eq!(reg.observe(), vec![Value::new(0, 4)]);

        // Reset released: the clear is destructive, so the value stays 0 - the
        // old 9 is gone, not merely masked.
        reg.apply_async(&[Value::new(9, 4), Value::ONE, Value::ZERO]);
        assert_eq!(reg.observe(), vec![Value::new(0, 4)]);
    }

    #[test]
    fn test_async_reset_dominates_on_tick() {
        let mut reg = new_reg(4);
        reg.tick(&ins(Value::new(9, 4), Value::ONE));

        // reset=1 dominates write_enable=1/data=5: latches 0, not 5.
        assert_eq!(
            reg.tick(&[Value::new(5, 4), Value::ONE, Value::ONE]),
            vec![Value::new(0, 4)]
        );
    }

    #[test_case(Value::ZERO; "reset exactly zero")]
    #[test_case(Value::Floating; "reset floating")]
    #[test_case(Value::new(1, 2); "reset wrong width")]
    fn test_reset_only_activates_on_exactly_one(rst: Value) {
        let mut reg = new_reg(4);
        // reset not exactly ONE: apply_async leaves state alone...
        reg.tick(&ins(Value::new(6, 4), Value::ONE));
        reg.apply_async(&[Value::new(9, 4), Value::ONE, rst]);
        assert_eq!(reg.observe(), vec![Value::new(6, 4)]);
        // ...and a normal write_enable=1 latch still proceeds through tick.
        assert_eq!(
            reg.tick(&[Value::new(5, 4), Value::ONE, rst]),
            vec![Value::new(5, 4)]
        );
    }

    #[test]
    fn test_multi_tick_sequence() {
        let mut reg = new_reg(4);

        // tick 1: we=1, data=3 -> latches 3.
        assert_eq!(
            reg.tick(&[Value::new(3, 4), Value::ONE, NO_RST]),
            vec![Value::new(3, 4)]
        );

        // tick 2: we=0, data=9 -> holds 3.
        assert_eq!(
            reg.tick(&[Value::new(9, 4), Value::ZERO, NO_RST]),
            vec![Value::new(3, 4)]
        );

        // tick 3: we=1, data=9 -> latches 9.
        assert_eq!(
            reg.tick(&[Value::new(9, 4), Value::ONE, NO_RST]),
            vec![Value::new(9, 4)]
        );
    }
}

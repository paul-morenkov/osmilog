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
